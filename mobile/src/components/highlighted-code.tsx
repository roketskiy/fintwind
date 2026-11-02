import { memo, useEffect, useState } from "react";
import "./syntax-highlight.css";

type Tokens = { text: string; className: string }[];
let worker: Worker | null = null;
let sequence = 0;
const pending = new Map<number, (tokens: Tokens | null) => void>();

function highlight(code: string, language: string, complete: (tokens: Tokens | null) => void) {
  try {
    if (!worker) {
      const instance = new Worker(new URL("./code-highlight.worker.ts", import.meta.url), { type: "module" });
      worker = instance;
      instance.onmessage = ({ data }: MessageEvent<{ id: number; tokens: Tokens | null }>) => {
        const callback = pending.get(data.id);
        pending.delete(data.id);
        callback?.(data.tokens);
      };
      instance.onerror = (event) => {
        event.preventDefault();
        instance.terminate();
        worker = null;
        const callbacks = [...pending.values()];
        pending.clear();
        callbacks.forEach((callback) => callback(null));
      };
    }
    const id = ++sequence;
    pending.set(id, complete);
    try {
      worker.postMessage({ id, code, language });
    } catch {
      pending.delete(id);
      complete(null);
    }
    return () => { pending.delete(id); };
  } catch {
    complete(null);
    return () => {};
  }
}

export const HighlightedCode = memo(function HighlightedCode({ code, language = "" }: { code: string; language?: string }) {
  const normalized = language.toLowerCase();
  const [result, setResult] = useState<{ code: string; language: string; tokens: Tokens } | null>(null);
  useEffect(() => {
    if (!normalized || code.length > 32768) return;
    let current = true;
    let cancel: (() => void) | undefined;
    // Streaming changes remain plain text until a short pause. Tokenization is
    // shared across blocks and runs off the UI thread, with no network requests.
    const timer = window.setTimeout(() => {
      cancel = highlight(code, normalized, (tokens) => {
        if (current && tokens) setResult({ code, language: normalized, tokens });
      });
    }, 120);
    return () => { current = false; window.clearTimeout(timer); cancel?.(); };
  }, [code, normalized]);
  const tokens = result?.code === code && result.language === normalized ? result.tokens : null;
  return <span className="syntax-code">{tokens
    ? tokens.map((token, index) => <span key={index} className={token.className || undefined}>{token.text}</span>)
    : code}</span>;
});
