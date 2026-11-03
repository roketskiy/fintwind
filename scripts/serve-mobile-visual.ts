// Static, local-only preview. Owns no child processes and expires after 15 minutes.
import { resolve, extname, sep } from "node:path";
const root = resolve(import.meta.dir, "../target/mobile-visual");
const mime: Record<string, string> = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml" };
const server = Bun.serve({
  hostname: "127.0.0.1", port: 0,
  async fetch(request) {
    const url = new URL(request.url);
    const path = resolve(root, `.${decodeURIComponent(url.pathname === "/" ? "/visual.html" : url.pathname)}`);
    if (!path.startsWith(root + sep)) return new Response("Not found", { status: 404 });
    const file = Bun.file(path);
    if (!(await file.exists())) return new Response("Not found", { status: 404 });
    // Workflow checks use the production style/script restrictions. Other
    // existing preview pages keep their standalone, inline bootstrap support.
    const workflow = url.searchParams.has("workflow");
    let csp = workflow
      ? "default-src 'none'; style-src 'self'; style-src-attr 'unsafe-inline'; script-src 'self'; connect-src 'none'; img-src 'self' blob: data:; font-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"
      : "default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'none'; img-src 'self' blob: data:; frame-src 'self'";
    // Only the isolated sizing fixture can embed a local workflow page. Script,
    // style, font and network restrictions stay identical to the strict check.
    if (workflow && url.searchParams.has("frame")) csp = csp.replace("frame-ancestors 'none'", "frame-ancestors 'self'");
    return new Response(file, { headers: { "content-type": mime[extname(path)] ?? "application/octet-stream", "cache-control": "no-store", "content-security-policy": csp } });
  },
});
console.log(`Visual preview: http://127.0.0.1:${server.port}/visual.html`);
setTimeout(() => { server.stop(true); }, 15 * 60 * 1000);
