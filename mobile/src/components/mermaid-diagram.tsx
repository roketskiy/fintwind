import { memo, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { CodeBlockCopyButton } from "@/components/ui/code-block";

type Drawing = { svg: string; duration: number };
const drawings = new Map<string, Promise<Drawing>>();
const images = new Map<string, string>();
let renderer: Promise<[typeof import("pele/lazy"), typeof import("dompurify")]> | undefined;
let ordinal = 0;

function palette(host: HTMLElement) {
  const style = getComputedStyle(host);
  const variables: Record<string, string> = {};
  for (const [name, token] of Object.entries({ bg: "background", fg: "foreground", muted: "muted-foreground", line: "muted-foreground",
    surface: "card", "surface-alt": "muted", border: "border", accent: "primary", critical: "destructive", error: "destructive" })) {
    variables[`--pele-${name}`] = style.getPropertyValue(`--${token}`).trim();
  }
  for (let n = 1; n <= 8; n++) variables[`--pele-series-${n}`] = style.getPropertyValue(`--chart-${(n - 1) % 5 + 1}`).trim();
  return { variables, font: style.fontFamily };
}

async function renderDiagram(source: string, width: number, theme: ReturnType<typeof palette>): Promise<{ image: string; duration: number }> {
  if (source.length > 16_384 || /%%\{|^\s*---|<\/?[a-z!]|(?:https?:|javascript:|data:|url\s*\()|\b(?:click|image|icon)\b/i.test(source)) throw new Error("图表包含不支持的内容");
  const key = JSON.stringify([source, width, theme.font]);
  const imageKey = JSON.stringify([key, theme.variables]);
  const cached = images.get(imageKey);
  if (cached) return { image: cached, duration: 0 };
  renderer ??= Promise.all([import("pele/lazy"), import("dompurify")]);
  const [pele, { default: DOMPurify }] = await renderer;
  let drawing = drawings.get(key);
  if (!drawing) {
    drawing = (async () => {
      const started = performance.now();
      // Native conversation diagrams use the host's Ondine token names. Map
      // only the documented palette names; the original source stays copyable.
      // Pele's style grammar accepts literal colors, but not var(...). Use
      // temporary colors during layout, then restore host variables in SVG.
      const colors = new Map<string, string>();
      const compatible = source.replace(/\b(fill|stroke|color):\s*var\(--(?:ondine|pele)-(bg|fg|muted|line|surface-alt|surface|border|accent|critical|series-[1-8])\)/g,
        (_match, property: string, name: string) => {
          let color = "";
          let index = colors.size;
          do { color = `#${(0xfa0100 + index++).toString(16)}`; } while (source.toLowerCase().includes(color) || colors.has(color));
          colors.set(color, `var(--pele-${name})`);
          return `${property}:${color}`;
        });
      const result = await pele.renderAsync(compatible, { maxWidth: width, fontFamily: theme.font, fontSize: 14,
        idPrefix: `phone-pele-${++ordinal}`, links: false, images: false,
        limit: 16_384, maxEdges: 200, outputLimit: 500_000 });
      const svg = result.svg.replace(/#[0-9a-f]{6}\b/gi, color => colors.get(color.toLowerCase()) ?? color);
      return { svg, duration: performance.now() - started };
    })();
    drawings.set(key, drawing);
    while (drawings.size > 8) drawings.delete(drawings.keys().next().value!);
    void drawing.catch(() => { if (drawings.get(key) === drawing) drawings.delete(key); });
  }
  const result = await drawing;
  // Never insert live, provider-authored SVG. The palette comes only from the
  // host theme; styles inside the inert image need no page-level CSP exception.
  const safe = DOMPurify.sanitize(result.svg, { USE_PROFILES: { svg: true, svgFilters: true },
    FORBID_TAGS: ["foreignObject", "script", "a", "image", "use"], FORBID_ATTR: ["href", "xlink:href"] });
  const document = new DOMParser().parseFromString(safe, "image/svg+xml");
  const svg = document.documentElement;
  if (svg.tagName !== "svg") throw new Error("无效图表");
  for (const [name, value] of Object.entries(theme.variables)) if (value) svg.style.setProperty(name, value);
  svg.style.setProperty("--pele-font", theme.font);
  const image = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(new XMLSerializer().serializeToString(svg))}`;
  if (image.length > 2_000_000) throw new Error("图表过大");
  images.set(imageKey, image);
  while (images.size > 8) images.delete(images.keys().next().value!);
  return { image, duration: result.duration };
}

export const MermaidDiagram = memo(function MermaidDiagram({ source }: { source: string }) {
  const host = useRef<HTMLElement>(null);
  const [width, setWidth] = useState(0);
  const [visible, setVisible] = useState(false);
  const [themeVersion, setThemeVersion] = useState(0);
  const [result, setResult] = useState<{ source: string; image?: string; failed?: boolean; duration?: number } | null>(null);
  const [showSource, setShowSource] = useState(false);
  useEffect(() => {
    const element = host.current;
    if (!element) return;
    const size = new ResizeObserver(entries => {
      const entry = entries[0];
      if (entry) setWidth(Math.max(120, Math.round(entry.contentRect.width - 16)));
    });
    const observer = new MutationObserver(() => setThemeVersion(value => value + 1));
    const visibility = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) { setVisible(true); visibility.disconnect(); }
    }, { rootMargin: "200px" });
    size.observe(element);
    visibility.observe(element);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => { size.disconnect(); observer.disconnect(); visibility.disconnect(); };
  }, []);
  useEffect(() => {
    if (!width || !visible || !host.current) return;
    let active = true;
    // No fixed delay or DOM measurement scaffold. Unchanged sources share the
    // in-flight drawing, and theme changes recolor the cached layout only.
    void renderDiagram(source, width, palette(host.current)).then(
      value => { if (active) setResult({ source, ...value }); },
      () => { if (active) setResult({ source, failed: true }); },
    );
    return () => { active = false; };
  }, [source, width, visible, themeVersion]);
  const ready = result?.source === source ? result : null;
  return <figure ref={host} className="mermaid-diagram" aria-label="Mermaid 图表" data-renderer="pele" data-render-ms={ready?.duration}>
    <figcaption className="mermaid-actions"><span>Mermaid</span><Button type="button" variant="ghost" size="sm" onClick={() => setShowSource(value => !value)}>{showSource ? "图表" : "源码"}</Button><CodeBlockCopyButton value={source} /></figcaption>
    {ready?.image && !showSource ? <div className="mermaid-scroll" tabIndex={0} aria-label="图表"><img src={ready.image} alt="Mermaid 图表" /></div>
      : showSource || ready?.failed ? <><p className="composer-notice">{ready?.failed ? "图表暂无法渲染" : null}</p><pre><code>{source}</code></pre></>
      : <p role="status" className="composer-notice">正在生成图表…</p>}
  </figure>;
});
