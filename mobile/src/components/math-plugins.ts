import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import "katex/dist/katex.min.css";
import type { PluggableList } from "unified";
export { normalizeTexMath } from "@/lib/tex-math";

export const remarkPlugins = [remarkGfm, remarkMath];
export const rehypePlugins: PluggableList = [[rehypeKatex, {
  trust: false, strict: "ignore", maxExpand: 200, maxSize: 20, throwOnError: false,
}]];
