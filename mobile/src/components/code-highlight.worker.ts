import { common, createLowlight } from "lowlight";

const highlighter = createLowlight(common);
type Nodes = ReturnType<typeof highlighter.highlight>["children"];

self.onmessage = ({ data }: MessageEvent<{ id: number; code: string; language: string }>) => {
  const { id, code, language } = data;
  const tokens: { text: string; className: string }[] = [];
  try {
    if (code.length > 32768 || !highlighter.registered(language)) {
      self.postMessage({ id, tokens: null });
      return;
    }
    function visit(nodes: Nodes, classes: string[] = []) {
      for (const node of nodes) {
        if (tokens.length > 4096) throw new Error("Token limit");
        if (node.type === "text") tokens.push({ text: node.value, className: classes.join(" ") });
        else if (node.type === "element") {
          const names = node.properties.className;
          visit(node.children, [...classes, ...(Array.isArray(names) ? names.map(String) : [])]);
        }
      }
    }
    visit(highlighter.highlight(language, code).children);
    // Never replace the source with a lossy tokenization.
    self.postMessage({ id, tokens: tokens.map((token) => token.text).join("") === code ? tokens : null });
  } catch {
    self.postMessage({ id, tokens: null });
  }
};
