/**
 * Bump the pinned Ondine revision to the current main of wevm/ondine.
 *
 * Fintwind compiles Mermaid with Ondine (package `compiler`, pinned by
 * revision). Upstream is young and moves fast; this script is the first half
 * of the update ritual, the mechanical part. The judgment stays with you:
 *
 *   1. bun scripts/update-ondine.ts
 *   2. cargo update --offline -p compiler
 *   3. cargo run --locked --offline --package fintwind-diagram --example diagram_blackbox -- C:/Users/Public/Temp/opencode/diagram-report.json
 *      (run it twice; the two reports must stay byte-identical)
 *   4. cargo run --locked --offline --package fintwind-core --example mermaid_check_e2e -- C:/Users/Public/Temp/opencode/mermaid-check-report.json
 *   5. Render a few real diagrams in the app: a flowchart, a sequence, a
 *      pie, and a Gantt (the reference day must stay authoritative).
 *
 * The script never builds or commits. A changed report fingerprint means the
 * renderer's output changed; review `docs/diagrams.md` and update the theme
 * mapping notes when the diff is wanted.
 */

const CARGO_FILE = "crates/fintwind-diagram/Cargo.toml";
const REPO = "wevm/ondine";
const TIMEOUT_MS = 30_000;
const PROXY = "http://127.0.0.1:7897";

function fail(message: string): never {
  console.error(`update-ondine: ${message}`);
  process.exit(1);
}

async function headRevision(): Promise<string> {
  const url = `https://api.github.com/repos/${REPO}/commits/main`;
  const proxies = [process.env.HTTPS_PROXY, process.env.HTTP_PROXY, PROXY];
  let lastError: unknown;
  for (const proxy of proxies) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
    try {
      const response = await fetch(url, {
        signal: controller.signal,
        headers: { "user-agent": "fintwind-update-ondine" },
        // Bun honors a per-fetch proxy through env; direct attempts use the
        // explicit local proxy as the last resort.
        ...(proxy ? { proxy } : {}),
      } as RequestInit & { proxy?: string });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const payload = (await response.json()) as { sha?: string };
      if (typeof payload.sha !== "string" || !/^[0-9a-f]{40}$/.test(payload.sha)) {
        throw new Error("the API answer carried no commit sha");
      }
      return payload.sha;
    } catch (error) {
      lastError = error;
    } finally {
      clearTimeout(timer);
    }
  }
  fail(`could not read the head revision of ${REPO}: ${String(lastError)}`);
}

const revision = await headRevision();

const text = await Bun.file(CARGO_FILE).text();
const pattern = /(ondine = \{ package = "compiler", git = "https:\/\/github\.com\/wevm\/ondine", rev = ")([0-9a-f]{40})(")/;
if (!pattern.test(text)) {
  fail(`no pinned ondine revision found in ${CARGO_FILE}`);
}
const current = text.match(pattern)?.[2];
if (current === revision) {
  console.log(`update-ondine: already at ${revision}`);
  process.exit(0);
}
await Bun.write(CARGO_FILE, text.replace(pattern, `$1${revision}$3`));
console.log(`update-ondine: pinned ${current} -> ${revision}`);
console.log("update-ondine: now run the ritual in the header comment (cargo update, blackbox twice, mermaid_check_e2e, visual check).");
