/**
 * Fintwind browser PoC fixture (phase 1) — offline and self-contained.
 *
 * `startFixture(runId)` starts two loopback HTTP servers (Bun.serve) and returns
 * { origin (main), stop }: main serves /page/alpha|beta|gamma?run=<runId> and
 * /popup?run=<runId>; frame serves /frame?run=<runId> and is embedded by every
 * page (main handler is wired with the frame origin, so framing is cross-origin
 * by port only).
 *
 * Failure modes this fixture exists to surface:
 * - iframe/cross-origin: framing blocked, frame document unreachable, frame
 *   controls unreachable via frame locators/CDP, or frame clicks untrusted.
 * - trusted input: window.pocEvents records Event.isTrusted of #count's click
 *   and #name's input; failures = synthesized events or events lost to late
 *   listener attachment.
 * - autoWait: #delayed appears 500ms after clicking #show-delayed, and the
 *   overlay over #blocked is removed 500ms after clicking #show-blocked — never
 *   on a page-load timer — so auto-waiting must wait for real state changes.
 * - console/network: #debug fires console.error('fintwind-poc-console') and a
 *   deliberately 404ing fetch('/missing-resource') that is never awaited, so a
 *   missing rejection handler would surface as an unhandled rejection.
 *
 * Hardening: exact route matching, UUID-validated runIds, fixed pageIds, HTML
 * escaping, no-store HTML, no cookies set (cookie presence read-only, never
 * logged), no external hosts, no dynamic deps, no command execution.
 */

export const PAGE_IDS = ["alpha", "beta", "gamma"] as const;
export type PageId = (typeof PAGE_IDS)[number];

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const HTML_HEADERS = {
  "content-type": "text/html; charset=utf-8",
  "cache-control": "no-store",
} as const;

function isPageId(value: string): value is PageId {
  return (PAGE_IDS as readonly string[]).includes(value);
}

function html(status: number, body: string): Response {
  return new Response(body, { status, headers: { ...HTML_HEADERS } });
}

function notFound(): Response {
  return html(404, "not found");
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function frameHtml(): string {
  return `<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Cross-origin fixture</title></head>
<body data-page-id="frame">
<button id="frame-count" type="button">frame count</button>
<span id="frame-value" data-count="0">0</span>
<script>
(function () {
  var count = 0;
  document.getElementById("frame-count").addEventListener("click", function () {
    count += 1;
    var out = document.getElementById("frame-value");
    if (out) { out.textContent = String(count); out.setAttribute("data-count", String(count)); }
  });
})();
</script>
</body>
</html>`;
}

function popupHtml(runId: string): string {
  return `<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Fintwind PoC Popup</title></head>
<body data-page-id="popup">
<p id="popup-marker">popup ${escapeHtml(runId)}</p>
</body>
</html>`;
}

function pageHtml(pageId: PageId, runId: string, frameOrigin: string): string {
  const safePageId = escapeHtml(pageId);
  const safeRun = escapeHtml(runId);
  const safeFrame = escapeHtml(frameOrigin);
  return `<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Fintwind PoC ${safePageId}</title>
<style>
#overlay-wrap { position: relative; display: inline-block; }
#overlay { position: absolute; inset: 0; background: ButtonFace; }
</style>
</head>
<body data-page-id="${safePageId}">
<button id="count" type="button">count</button> <span id="count-value" data-count="0">0</span>
<input id="name" type="text" /> <span id="name-value"></span>
<button id="show-delayed" type="button">show delayed</button> <button id="delayed" type="button" hidden>delayed</button> <span id="delayed-result">not-clicked</span>
<button id="show-blocked" type="button">show blocked</button>
<span id="overlay-wrap"><button id="blocked" type="button">blocked</button><span id="overlay"></span></span>
<span id="blocked-result">not-clicked</span>
<a id="route" href="#routed">route</a> <a id="popup" href="/popup?run=${safeRun}" target="_blank">popup</a>
<button id="debug" type="button">debug</button> <span id="cookie-present">false</span>
<iframe title="Cross-origin fixture" src="${safeFrame}/frame?run=${safeRun}"></iframe>
<script>
(function () {
  var pageId = document.body.getAttribute("data-page-id") || "";
  var count = 0;
  window.pocEvents = { clickTrusted: false, inputTrusted: false };
  function byId(id) { return document.getElementById(id); }
  function setText(id, text) { var el = byId(id); if (el) { el.textContent = text; } }
  byId("count").addEventListener("click", function (event) {
    count += 1;
    window.pocEvents.clickTrusted = Boolean(event && event.isTrusted);
    var out = byId("count-value");
    if (out) { out.textContent = String(count); out.setAttribute("data-count", String(count)); }
  });
  byId("name").addEventListener("input", function (event) {
    window.pocEvents.inputTrusted = Boolean(event && event.isTrusted);
    var target = event.target;
    setText("name-value", String(target && target.value) || "");
  });
  byId("show-delayed").addEventListener("click", function () {
    setTimeout(function () { var el = byId("delayed"); if (el) { el.hidden = false; } }, 500);
  });
  byId("delayed").addEventListener("click", function () { setText("delayed-result", "clicked"); });
  byId("show-blocked").addEventListener("click", function () {
    setTimeout(function () {
      var overlay = byId("overlay");
      if (overlay && overlay.parentNode) { overlay.parentNode.removeChild(overlay); }
    }, 500);
  });
  byId("blocked").addEventListener("click", function () { setText("blocked-result", "clicked"); });
  byId("route").addEventListener("click", function (event) {
    event.preventDefault();
    var next = new URL(window.location.href);
    next.hash = "routed";
    window.history.pushState(null, "", next.toString());
    document.title = "Fintwind PoC " + pageId + " Route";
  });
  byId("debug").addEventListener("click", function () {
    console.error("fintwind-poc-console");
    fetch("/missing-resource").catch(function () {});
  });
  setText("cookie-present", document.cookie.length > 0 ? "true" : "false");
})();
</script>
</body>
</html>`;
}

export function startFixture(runId: string): { origin: string; stop: () => void } {
  if (!UUID_RE.test(runId)) {
    throw new Error(`startFixture: runId must be a UUID, got: ${runId}`);
  }

  // Frame server starts first so the main handler can embed its origin.
  const frame = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request: Request): Response {
      const url = new URL(request.url);
      if (url.pathname !== "/frame" || url.searchParams.get("run") !== runId) {
        return notFound();
      }
      return html(200, frameHtml());
    },
  });
  const frameOrigin = `http://127.0.0.1:${String(frame.port)}`;

  const main = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request: Request): Response {
      const url = new URL(request.url);
      if (request.method !== "GET" && request.method !== "HEAD") {
        return notFound();
      }
      if (url.pathname === "/popup" && url.searchParams.get("run") === runId) {
        return html(200, popupHtml(runId));
      }
      const match = /^\/page\/([a-z]+)$/.exec(url.pathname);
      const rawPageId = match === null ? undefined : match[1];
      if (rawPageId === undefined || !isPageId(rawPageId)) {
        return notFound();
      }
      if (url.searchParams.get("run") !== runId) {
        return notFound();
      }
      return html(200, pageHtml(rawPageId, runId, frameOrigin));
    },
  });

  return {
    origin: `http://127.0.0.1:${String(main.port)}`,
    stop(): void {
      main.stop(true);
      frame.stop(true);
    },
  };
}
