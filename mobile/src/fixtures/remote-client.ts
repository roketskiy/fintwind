// Public client boundary substitute for the real hook/view E2E fixture.
// It cannot open sockets, pair, read files, or call a daemon.
import type { ClientSnapshot, CommandMap, CommandType, RuntimeMode, SessionDetail } from "../lib/types";
import { checkFieldBounds } from "../lib/remote-validation";

const params = new URLSearchParams(location.search);
const permissions = params.get("permissions") === "1";
let runtimeMode: RuntimeMode = "ask";
const configurations: CommandMap["configureSession"]["fields"][] = [];
const noop = () => {};
const historical = Array.from({ length: permissions ? 0 : params.get("activities") === "1" ? 20 : 60 }, (_, index) => [
  { id: `u${index}`, role: "user" as const, text: `历史问题 ${index}` },
  { id: `a${index}`, role: "assistant" as const, text: `历史回答 ${index}\n\n${"用于核对阅读位置的历史内容。".repeat(24)}` },
]).flat();
let tick = 0;
let hidden = false;
let failRead = false;
let failMutation = false;
let holdRead = false;
let releaseRead: (() => void) | null = null;
let failWatch: (() => void) | null = null;
let activityComplete = false;
let turnEnded = false;
let waiting = false;
let activityCount = params.get("activities") === "1" ? 100 : 1;
let snapshot: ClientSnapshot = { connection: "idle", error: null, pendingScopes: [] };
const listeners = new Set<() => void>();
const counters = { reads: 0, watches: 0, activeWatches: 0, mutations: 0, deliveries: 0 };
function publish() {
  document.documentElement.dataset.fixtureState = JSON.stringify({ ...counters, tick, hidden, held: releaseRead !== null, runtimeMode, configurations });
}
function detail(): SessionDetail {
  return { id: "fixture-session", projectId: "fixture-project", title: "隔离流式会话", status: permissions && params.get("busy") !== "1" ? "idle" : turnEnded ? "idle" : waiting ? "waiting" : "working", messages: [],
    features: permissions ? ["composerOptions", ...(params.get("legacy") === "1" ? [] : ["sessionDeltas", "sessionPermissions"])] : params.get("legacy") === "1" ? [] : ["sessionDeltas"],
    ...(permissions && params.get("legacy") !== "1" ? { runtimeMode } : {}),
    model: "fixture", modelName: "Fixture", interactionMode: "build", reasoningEffort: "high", truncated: false,
    permission: null, questions: null, desktopRequired: false, canSend: true,
    entries: [...historical,
      ...(params.get("footers") === "1" ? [
        { id: "live-prompt", role: "user" as const, text: "检查阶段性答复页脚", createdAt: 1793635200 },
        { id: "progress-before", role: "assistant" as const, text: "先检查实现，再执行验证。", createdAt: 1793635201 },
      ] : []),
      ...Array.from({ length: activityCount }, (_, index) => ({ id: `live-tool-${index}`, type: "activity" as const, kind: "tool", title: `隔离活动 ${index + 1}`, target: "", text: "", complete: activityComplete || index < activityCount - 1, failed: false, truncated: false })),
      ...(params.get("footers") === "1" ? [
        { id: "progress-after", role: "assistant" as const, text: "实现已完成，正在核对结果。", createdAt: 1793635202 },
        { id: "verify-tool", type: "activity" as const, kind: "tool", title: "验证", target: "", text: "", complete: true, failed: false, truncated: false },
      ] : []),
      { id: "live", role: "assistant", text: `更新 ${tick}\n\n${"持续输出的内容。\n".repeat(Math.min(tick, 180))}`, streaming: !turnEnded, createdAt: 1793635210,
        ...(turnEnded ? { elapsedSeconds: 10 } : {}) },
    ] };
}
function phase(connection: ClientSnapshot["connection"]) {
  snapshot = { ...snapshot, connection };
  listeners.forEach((listener) => listener());
}
export const remoteClient = {
  subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
  getSnapshot: () => snapshot,
  start: () => phase("connected"), stop: () => phase("idle"), reconnect: () => phase("connected"), forget: async () => phase("unpaired"),
  async command<K extends CommandType>(type: K, _fields: CommandMap[K]["fields"]): Promise<CommandMap[K]["result"]> {
    let result: unknown;
    if (type === "listWorkspaces") result = { workspaces: [{ id: "fixture-project", name: "Fixture" }], selectedProjectId: "fixture-project" };
    else if (type === "listSessions") result = { sessions: [], cutoff: 0 };
    else if (type === "getSession") {
      counters.reads++;
      publish();
      if (holdRead) {
        holdRead = false;
        await new Promise<void>((resolve, reject) => {
          const timer = window.setTimeout(() => { releaseRead = null; reject(new Error("Fixture read timed out")); }, 10000);
          releaseRead = () => { window.clearTimeout(timer); releaseRead = null; publish(); resolve(); };
          publish();
        });
      }
      if (failRead) { failRead = false; throw new Error("隔离读取失败"); }
      result = detail();
    } else if (type === "getComposer") result = { canConfigure: permissions, models: [], commands: [] };
    else {
      counters.mutations++;
      if (failMutation) { failMutation = false; throw new Error("隔离操作结果待确认"); }
      if (type === "configureSession") {
        checkFieldBounds(type, _fields);
        if (!permissions || detail().status !== "idle") throw new Error("会话当前不可配置");
        const fields = _fields as CommandMap["configureSession"]["fields"];
        if (fields.sessionId !== "fixture-session") throw new Error("会话不匹配");
        if (fields.runtimeMode) runtimeMode = fields.runtimeMode;
        configurations.push({ ...fields });
        publish();
      }
      result = { accepted: true };
    }
    return result as CommandMap[K]["result"];
  },
  watchSession(_id: string, receive: (value: SessionDetail) => void, fail: (error: Error) => void) {
    counters.watches++; counters.activeWatches++;
    publish();
    const timer = window.setInterval(() => { tick++; counters.deliveries++; publish(); receive(detail()); }, 200);
    failWatch = () => fail(new Error("隔离增量缺失"));
    let active = true;
    return () => { if (!active) return; active = false; counters.activeWatches--; window.clearInterval(timer); failWatch = null; publish(); };
  },
};

// Only this isolated fixture changes visibility, never the user's real page.
Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
export const fixture = {
  counters,
  completeActivity: () => { activityComplete = true; },
  appendActivity: () => { activityCount++; },
  finishTurn: () => { activityComplete = true; turnEnded = true; },
  waitForApproval: () => { activityComplete = true; waiting = true; },
  failRead: () => { failRead = true; },
  holdRead: () => { holdRead = true; },
  releaseRead: () => releaseRead?.(),
  failMutation: () => { failMutation = true; },
  failWatch: () => (failWatch ?? noop)(),
  visibility(value: boolean) { hidden = value; document.dispatchEvent(new Event("visibilitychange")); publish(); },
  state: () => ({ ...counters, tick, hidden, held: releaseRead !== null }),
};
