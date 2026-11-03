// Error text is plain text, bounded, and must not expose common credentials.
// The desktop additionally removes exact configured API keys before projection.
export function errorMessage(value: unknown, fallback = "操作失败。"): string {
  const text = value instanceof Error ? value.message : typeof value === "string" ? value : fallback;
  return text
    .replace(/https?:\/\/[^\s"'<>]+/gi, "[地址已隐藏]")
    .replace(/\b(?:Bearer|Basic)\s+[^\s"',;]+/gi, "[凭据已隐藏]")
    .replace(/((?:api[_-]?key|authorization|access[_-]?token|password|secret)["']?\s*[:=]\s*)["']?[^\s"',;&]+/gi, "$1[凭据已隐藏]")
    .replace(/\bsk-[a-z0-9_-]{8,}/gi, "[凭据已隐藏]")
    .slice(0, 4096) || fallback;
}
