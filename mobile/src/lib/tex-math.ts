import { fromMarkdown } from "mdast-util-from-markdown";
import { math } from "micromark-extension-math";
import { mathFromMarkdown } from "mdast-util-math";
import type { Nodes } from "mdast";

const protectedTypes = new Set(["code", "inlineCode", "math", "inlineMath", "html", "link", "image", "definition"]);
const displayEnvironment = /^\\begin\{(equation\*?|align\*?|gather\*?|multline\*?|displaymath)\}/;

// CommonMark consumes \( and \[ as punctuation escapes before remark-math can
// see them. Normalize complete TeX pairs first, protecting actual Markdown code,
// link destinations and existing math using the parser's source offsets.
export function normalizeTexMath(source: string): string {
  if (!/\\(?:\[|\(|begin\{)|\$\$[^\n]/.test(source)) return source;
  const ranges: [number, number][] = [];
  const singleLineDisplay = new Map<number, number>();
  const tree = fromMarkdown(source, { extensions: [math()], mdastExtensions: [mathFromMarkdown()] });
  function collect(node: Nodes) {
    if (protectedTypes.has(node.type)) {
      const start = node.position?.start.offset, end = node.position?.end.offset;
      if (start !== undefined && end !== undefined) {
        ranges.push([start, end]);
        const raw = source.slice(start, end);
        const before = source.slice(source.lastIndexOf("\n", start - 1) + 1, start);
        const after = source.slice(end, source.indexOf("\n", end) < 0 ? source.length : source.indexOf("\n", end));
        if (node.type === "inlineMath" && /^\$\$[^\n]*\$\$$/.test(raw) && /^[ \t>]*$/.test(before) && !after.trim()) singleLineDisplay.set(start, end);
      }
    } else if ("children" in node) node.children.forEach(collect);
  }
  collect(tree);
  let rangeIndex = 0, copied = 0, index = 0;
  const output: string[] = [];
  function escaped(at: number) {
    let count = 0;
    while (at > 0 && source[--at] === "\\") count++;
    return count % 2 === 1;
  }
  function findClose(token: string, start: number, boundary: number) {
    let found = source.indexOf(token, start);
    while (found >= 0 && found < boundary && escaped(found)) found = source.indexOf(token, found + token.length);
    return found >= 0 && found + token.length <= boundary ? found : -1;
  }
  function display(body: string, start: number, end: number) {
    const before = source.slice(source.lastIndexOf("\n", start - 1) + 1, start);
    const quotePrefix = /^[ \t]*(?:>[ \t]*)*/.exec(before)?.[0] ?? "";
    const listMarker = /^(?:[-+*]|\d+[.)])[ \t]+/.exec(before.slice(quotePrefix.length))?.[0];
    const prefix = quotePrefix + (listMarker ? " ".repeat(listMarker.length) : "");
    const onOwnLine = before === quotePrefix || before === quotePrefix + (listMarker ?? "");
    const lines = body.split("\n").map(line => prefix && line.startsWith(prefix) ? line.slice(prefix.length) : line);
    const content = lines.join("\n").trim();
    const after = source.slice(end, source.indexOf("\n", end) < 0 ? source.length : source.indexOf("\n", end));
    return `${onOwnLine ? "" : `\n${prefix}`}$$\n${content.split("\n").map(line => prefix + line).join("\n")}\n${prefix}$$${after.trim() ? `\n${prefix}` : ""}`;
  }

  while (index < source.length) {
    while (ranges[rangeIndex] && ranges[rangeIndex]![1] <= index) rangeIndex++;
    const range = ranges[rangeIndex];
    if (range && index >= range[0]) {
      if (singleLineDisplay.has(index)) {
        output.push(source.slice(copied, index), display(source.slice(index + 2, range[1] - 2), index, range[1]));
        copied = range[1];
      }
      index = range[1]; continue;
    }
    if (source[index] !== "\\" || escaped(index)) { index++; continue; }
    const boundary = range?.[0] ?? source.length;
    const opening = source.slice(index, index + 2);
    const environment = displayEnvironment.exec(source.slice(index));
    let end = -1, length = 2, replacement = "";
    if (opening === "\\(" || opening === "\\[") {
      end = findClose(opening === "\\(" ? "\\)" : "\\]", index + 2, boundary);
      if (end >= 0) {
        const body = source.slice(index + 2, end);
        // Do not turn unrelated paragraphs into one unfinished inline formula.
        if (opening === "\\(" && /\n[ \t]*\n/.test(body)) { index += 2; continue; }
        replacement = opening === "\\(" ? `$${body.trim()}$` : display(body, index, end + 2);
      }
    } else if (environment) {
      const close = `\\end{${environment[1]}}`;
      end = findClose(close, index + environment[0].length, boundary);
      length = close.length;
      if (end >= 0) replacement = display(source.slice(index, end + length), index, end + length);
    }
    if (end < 0) { index++; continue; }
    output.push(source.slice(copied, index), replacement);
    index = end + length;
    copied = index;
  }
  if (!output.length) return source;
  output.push(source.slice(copied));
  return output.join("");
}
