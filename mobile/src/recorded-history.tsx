// Read-only rendering of a locally injected real projection. No remote client,
// pairing, synthetic messages, mutation controls, or inferred tool transitions.
import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { Conversation } from "./components/conversation";
import { Composer } from "./components/composer";
import { parseResult } from "./lib/remote-validation";
import { applyTheme } from "./lib/theme";
import type { SessionDetail } from "./lib/types";
import "./index.css";

const params = new URLSearchParams(location.search);
applyTheme(params.get("theme") === "dark" ? "dark" : "light");
const payload: unknown = JSON.parse(document.getElementById("recorded-history")?.textContent ?? "null");
const session = payload === null ? null : parseResult("getSession", payload) as SessionDetail;
function noop() {}
async function noMutation() { return null; }
function RecordedHistory({ session }: { session: SessionDetail }) {
  const entries = session.entries ?? session.messages;
  const through = Number(params.get("through"));
  const initial = Number.isSafeInteger(through) && through > 0 ? Math.min(through, entries.length) : entries.length;
  const [count, setCount] = useState(initial);
  useEffect(() => {
    if (params.get("replay") !== "1") return;
    const requestedStep = Number(params.get("stepMs"));
    const step = Number.isFinite(requestedStep) && requestedStep >= 200 ? Math.min(requestedStep, 5000) : 500;
    const timer = window.setInterval(() => setCount((value) => Math.min(value + 1, entries.length)), step);
    return () => window.clearInterval(timer);
  }, [entries.length]);
  return <main style={{ height: "100dvh", width: "min(100%, 390px)", marginInline: "auto", display: "flex", flexDirection: "column" }} data-recorded-through={count} data-recorded-total={entries.length}>
    <Conversation session={{ ...session, entries: entries.slice(0, count), todos: params.get("replay") !== "1" && count === entries.length ? session.todos : null }} disabled busy={false} onPermission={noop} onAnswers={noop} onError={noop} />
    <Composer session={session} draft="" onDraft={noop} disabled busy={false} onError={noop} mutate={noMutation} />
  </main>;
}
createRoot(document.getElementById("root")!).render(session ? <RecordedHistory session={session} /> : <p>未载入会话记录。</p>);
