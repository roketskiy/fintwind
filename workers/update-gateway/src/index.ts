// Update gateway: one cached JSON for the desktop update check.
//
// Why this exists: the desktop used to page `api.github.com/repos/.../releases`
// anonymously on every launch. Anonymous API calls share 60 req/hour per IP,
// so office / campus NAT and mainland retries hit 403 rate-limit often.
// This Worker calls the same API with a server-side token, picks the newest
// stable release once, and caches the answer at the edge. Clients do one
// small GET and never touch the GitHub rate limit themselves.
//
// Routes:
//   GET /v1/latest?current=0.2.13  -> 200 JSON or 204 when already current
//   GET /v1/latest                 -> 200 JSON (newest stable, no comparison)
//   GET /healthz                   -> 200 JSON
//
// Response JSON (matches the desktop ReleaseInfo shape):
//   { version, notes, url, assets: [{ name, url, size, sha256 }] }
// where sha256 is bare hex (or null when the release published no digest).

const GITHUB_API = "https://api.github.com";
const CACHE_TTL_SECONDS = 300;

function json(data, status = 200, cacheable = false) {
  const headers = new Headers({ "Content-Type": "application/json; charset=utf-8" });
  if (cacheable) {
    headers.set(
      "Cache-Control",
      `public, max-age=${CACHE_TTL_SECONDS}, s-maxage=${CACHE_TTL_SECONDS}, stale-while-revalidate=60`,
    );
  } else {
    headers.set("Cache-Control", "no-store");
  }
  return new Response(JSON.stringify(data), { status, headers });
}

function normalizeVersion(tag) {
  return String(tag ?? "")
    .trim()
    .replace(/^[vV]+/, "");
}

function pickLatest(releases) {
  let best = null;
  for (const release of releases ?? []) {
    if (!release || release.draft || release.prerelease) continue;
    const tag = String(release.tag_name ?? "").trim();
    if (!tag) continue;
    const published = Date.parse(release.published_at ?? "");
    if (!Number.isFinite(published)) continue;
    const order = [published, Number(release.id) || 0];
    if (
      best === null ||
      order[0] > best.order[0] ||
      (order[0] === best.order[0] && order[1] > best.order[1])
    ) {
      best = { order, release };
    }
  }
  return best?.release ?? null;
}

function toGatewayPayload(release) {
  const assets = (release.assets ?? [])
    .filter((asset) => asset && typeof asset.name === "string" && asset.browser_download_url)
    .map((asset) => {
      let sha256 = null;
      const digest = typeof asset.digest === "string" ? asset.digest : "";
      const hex = digest.startsWith("sha256:") ? digest.slice("sha256:".length) : "";
      // Bare hex only; anything else reads as "no digest", same as the client.
      if (/^[0-9a-fA-F]{64}$/.test(hex)) sha256 = hex.toLowerCase();
      return {
        name: asset.name,
        url: asset.browser_download_url,
        size: typeof asset.size === "number" ? asset.size : 0,
        sha256,
      };
    });
  return {
    version: normalizeVersion(release.tag_name),
    notes: typeof release.body === "string" ? release.body : "",
    url:
      typeof release.html_url === "string" && release.html_url.trim()
        ? release.html_url
        : "https://github.com/roketskiy/fintwind/releases/latest",
    assets,
  };
}

async function fetchReleasesPage(env, repo, page) {
  const url = `${GITHUB_API}/repos/${repo}/releases?per_page=100&page=${page}`;
  const response = await fetch(url, {
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: `Bearer ${env.GITHUB_TOKEN}`,
      "User-Agent": "fintwind-update-gateway",
      "X-GitHub-Api-Version": "2022-11-28",
    },
    // One page must never stall the whole check.
    signal: AbortSignal.timeout(8000),
  });
  if (response.status === 403 || response.status === 429) {
    const retryAfter = response.headers.get("retry-after") ?? "";
    throw new Error(`github rate-limited: HTTP ${response.status} retry-after=${retryAfter}`);
  }
  if (!response.ok) throw new Error(`github API HTTP ${response.status}: ${url}`);
  return response.json();
}

async function loadLatest(env, repo) {
  // Publication order is authoritative (a corrected version number can be
  // numerically smaller), so pages are scanned until a short page arrives.
  // Ten pages (1000 releases) bound the work; beyond that the list call
  // itself is the problem, same contract as the desktop fallback.
  const all = [];
  for (let page = 1; page <= 10; page++) {
    const releases = await fetchReleasesPage(env, repo, page);
    if (!Array.isArray(releases)) throw new Error("github API returned no release list");
    all.push(...releases);
    if (releases.length < 100) break;
  }
  return pickLatest(all);
}

export default {
  async fetch(request, env, ctx) {
    try {
      if (!env.GITHUB_TOKEN) {
        console.error(JSON.stringify({ message: "missing GITHUB_TOKEN secret" }));
        return json({ error: "update gateway is not configured" }, 500);
      }
      const repo = env.GITHUB_REPO || "roketskiy/fintwind";
      const url = new URL(request.url);

      if (request.method !== "GET") return json({ error: "method not allowed" }, 405);
      if (url.pathname === "/healthz") {
        return json({ ok: true, repo }, 200, true);
      }
      if (url.pathname !== "/v1/latest") return json({ error: "not found" }, 404);

      const cache = caches.default;
      // One cache entry per repo: the `current` comparison is cheap and stays
      // outside the cached body so every client version shares the entry.
      const cacheKey = new Request(`https://update-gateway/v1/latest?repo=${encodeURIComponent(repo)}`, {
        method: "GET",
      });
      let cached = null;
      try {
        cached = await cache.match(cacheKey);
      } catch {
        cached = null;
      }

      let payload = null;
      try {
        const latest = await loadLatest(env, repo);
        if (latest) {
          payload = toGatewayPayload(latest);
          // 204-worthy, but still cache the payload for the next caller.
          ctx.waitUntil(
            cache.put(cacheKey, json(payload, 200, true).clone()).catch(() => {}),
          );
        }
      } catch (error) {
        console.error(
          JSON.stringify({
            message: "github fetch failed",
            error: error instanceof Error ? error.message : String(error),
          }),
        );
        // Serve stale on upstream failure so a GitHub blip never reads as
        // "no update" to every client at once.
        if (cached) return new Response(cached.body, cached);
        return json({ error: "upstream unavailable" }, 502);
      }

      if (!payload) {
        if (cached) return new Response(cached.body, cached);
        return json({ error: "no stable release published yet" }, 502);
      }

      const current = normalizeVersion(url.searchParams.get("current") ?? "");
      if (current && payload.version === current) {
        return new Response(null, { status: 204, headers: { "Cache-Control": "no-store" } });
      }
      console.log(JSON.stringify({ message: "latest served", version: payload.version, repo }));
      return json(payload, 200, true);
    } catch (error) {
      console.error(
        JSON.stringify({
          message: "unhandled error",
          error: error instanceof Error ? error.message : String(error),
          path: new URL(request.url).pathname,
        }),
      );
      return json({ error: "internal server error" }, 500);
    }
  },
} satisfies ExportedHandler<Env>;
