// Public client boundary substitute for the real hook/view E2E fixture.
// It cannot open sockets, pair, read files, or call a daemon.
import type { ClientSnapshot, CommandMap, CommandType, RuntimeMode, SessionDetail, TranscriptEntry } from "../lib/types";
import { checkFieldBounds } from "../lib/remote-validation";
import { workflowEnabled, workflowDetail, workflowTasks, workflowCatalog, workflowAnswer, workflowSelectModel, workflowCounters } from "./workflow";

const params = new URLSearchParams(location.search);
const permissions = params.get("permissions") === "1";
const steering = params.get("steering") === "1";
const folding = params.get("folding") === "1";
let foldingTurn = 1;
const usageScenario = params.get("usage");
let usageUpdated = false;
const readMs = Math.min(2000, Math.max(0, Number(params.get("readMs")) || 0));
const readTimes: { start: number; end?: number }[] = [];
let inFlightReads = 0;
let maxInFlightReads = 0;
let steerAck: ((accepted: boolean) => void) | null = null;
const steers: CommandMap["steerPrompt"]["fields"][] = [];
const deliveredSteers: { id: string; role: "user"; text: string; turnId: string; turnRunning: boolean }[] = [];
let runtimeMode: RuntimeMode = "ask";
const configurations: CommandMap["configureSession"]["fields"][] = [];
const noop = () => {};
const historical = Array.from({ length: permissions || steering || params.has("latency") ? 0 : params.get("activities") === "1" ? 20 : 60 }, (_, index) => [
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
let turnEnded = params.get("settled") === "1";
let waiting = false;
let activityCount = params.get("activities") === "1" ? 100 : 1;
let snapshot: ClientSnapshot = { connection: "idle", error: null, pendingScopes: [] };
const listeners = new Set<() => void>();
const counters = { reads: 0, watches: 0, activeWatches: 0, mutations: 0, deliveries: 0, stops: 0 };
function publish() {
  document.documentElement.dataset.fixtureState = JSON.stringify({ ...workflowCounters, ...counters, tick, hidden, held: releaseRead !== null, runtimeMode, configurations, readTimes, inFlightReads, maxInFlightReads, steers, steerPending: steerAck !== null });
}
function detail(): SessionDetail {
  return { id: "fixture-session", projectId: "fixture-project", title: "隔离流式会话", status: permissions && params.get("busy") !== "1" ? "idle" : turnEnded ? "idle" : waiting ? "waiting" : "working", messages: [],
    features: [...(permissions ? ["composerOptions", ...(params.get("legacy") === "1" ? [] : ["sessionDeltas", "sessionPermissions"])] : params.get("legacy") === "1" ? [] : ["sessionDeltas"]), ...(steering && params.get("legacy") !== "1" ? ["steering"] : [])],
    ...(steering ? { canSteer: !turnEnded && !steerAck && params.get("unsupported") !== "1" } : {}),
    ...(permissions && params.get("legacy") !== "1" ? { runtimeMode } : {}),
    model: "fixture", modelName: "Fixture", interactionMode: "build", reasoningEffort: "high",
    permission: null, questions: null, desktopRequired: false, canSend: true,
    truncated: folding && params.get("truncated") === "1",
    ...usageFixture(),
    entries: usageScenario === "empty" ? [] : folding ? foldingEntries() : [...historical,
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
        ...(steering ? { turnId: "fixture-live", turnRunning: !turnEnded } : {}),
        ...(turnEnded ? { elapsedSeconds: 10 } : {}) },
      ...deliveredSteers,
    ] };
}

function usageFixture(): Pick<SessionDetail, "usage" | "todos" | "workspaceChanges"> {
  if (!usageScenario) return {};
  const known = { contextTokens: usageUpdated ? 52_000 : 42_000, contextWindow: 200_000,
    totalTokens: usageUpdated ? 2_234_567 : 1_234_567,
    cacheReadTokens: usageUpdated ? 900_000 : 800_000, promptTokens: usageUpdated ? 1_200_000 : 1_000_000 };
  return {
    ...(usageScenario === "legacy" ? {} : { usage: usageScenario === "unknown"
      ? { contextTokens: null, contextWindow: null, totalTokens: null, cacheReadTokens: null, promptTokens: null }
      : usageScenario === "zero" ? { ...known, contextTokens: 0, totalTokens: 0, cacheReadTokens: 0, promptTokens: 0 }
      : usageScenario === "over" ? { ...known, contextTokens: 250_000 } : known }),
    ...(usageScenario === "only" || usageScenario === "empty" ? {} : {
      todos: { items: [{ content: "已完成的隔离任务", status: "completed" }, { content: "正在处理的隔离任务", status: "in_progress" }], completed: 1, total: 2, truncated: false },
      workspaceChanges: { additions: 120, deletions: 34 },
    }),
  };
}

function foldingEntries(): TranscriptEntry[] {
  const entries: TranscriptEntry[] = [];
  const legacy = params.get("legacy") === "1";
  for (let index = 0; index <= foldingTurn; index++) {
    const running = index === foldingTurn && !turnEnded;
    const prefix = `fold-${index}`;
    const metadata = legacy ? {} : { turnId: prefix, turnRunning: running };
    entries.push(
      { id: `${prefix}-prompt`, role: "user", text: `第 ${index} 轮问题`, ...metadata },
      { id: `${prefix}-progress`, role: "assistant", text: `第 ${index} 轮阶段性消息。\n\n${"过程内容，用于验证阅读位置。\n".repeat(20)}`, ...metadata, ...(!legacy ? { responseComplete: false } : {}) },
      { id: `${prefix}-tool`, type: "activity", kind: "tool", title: `第 ${index} 轮工具`, target: "", text: "", complete: !running || activityComplete, failed: false, truncated: false, ...metadata },
      ...(!legacy ? [{ id: `${prefix}-steer`, role: "user" as const, text: "同轮引导，不是新一轮。", ...metadata }] : []),
      { id: `${prefix}-progress-after`, role: "assistant", text: "继续检查，尚未给出最终回复。", ...metadata, ...(!legacy ? { responseComplete: false } : {}) },
      { id: `${prefix}-reasoning`, type: "activity", kind: "reasoning", title: "思考", target: "", text: "隔离思考内容。", complete: !running || activityComplete, failed: false, truncated: false, ...metadata },
    );
    if (running || params.get("noFinal") !== "1") entries.push({ id: `${prefix}-final`, role: "assistant", text: running ? `第 ${index} 轮正在回复 ${tick}` : `第 ${index} 轮最终回复。`, streaming: running, ...metadata, ...(!legacy ? { responseComplete: !running } : {}), ...(!running ? { elapsedSeconds: 10 } : {}) });
  }
  return params.get("truncated") === "1" ? entries.slice(2) : entries;
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
    if (type === "listWorkspaces") result = { workspaces: [{ id: "fixture-project", name: "Fixture" }, ...(workflowEnabled ? [{ id: "other-project", name: "Other workspace" }] : [])], selectedProjectId: "fixture-project", ...(workflowEnabled && params.get("legacy") !== "1" ? { features: ["taskActivity"] } : {}) };
    else if (type === "getTaskActivity") { result = { tasks: workflowEnabled ? workflowTasks() : [] }; publish(); }
    else if (type === "listSessions") result = { sessions: [], cutoff: 0 };
    else if (type === "getSession") {
      counters.reads++;
      const timing: { start: number; end?: number } = { start: performance.now() };
      readTimes.push(timing);
      if (readTimes.length > 64) readTimes.shift();
      maxInFlightReads = Math.max(maxInFlightReads, ++inFlightReads);
      publish();
      try {
      if (holdRead) {
        holdRead = false;
        await new Promise<void>((resolve, reject) => {
          const timer = window.setTimeout(() => { releaseRead = null; reject(new Error("Fixture read timed out")); }, 10000);
          releaseRead = () => { window.clearTimeout(timer); releaseRead = null; publish(); resolve(); };
          publish();
        });
      }
      if (failRead) { failRead = false; throw new Error("隔离读取失败"); }
      if (readMs) await new Promise<void>((resolve) => window.setTimeout(resolve, readMs));
      result = workflowEnabled ? workflowDetail((_fields as CommandMap["getSession"]["fields"]).sessionId) : detail();
      } finally { timing.end = performance.now(); inFlightReads--; publish(); }
    } else if (type === "getComposer") result = workflowEnabled ? workflowCatalog : { canConfigure: permissions, models: [], commands: [] };
    else {
      counters.mutations++;
      if (failMutation) { failMutation = false; throw new Error("隔离操作结果待确认"); }
      if (type === "answerQuestions" && workflowEnabled) { await workflowAnswer(); publish(); }
      if (type === "stop" && steering) {
        counters.stops++; turnEnded = true; steerAck?.(false); publish();
      }
      if (type === "steerPrompt") {
        checkFieldBounds(type, _fields);
        const fields = _fields as CommandMap["steerPrompt"]["fields"];
        if (!steering || !detail().canSteer || fields.sessionId !== "fixture-session") throw new Error("当前会话无法接收引导。");
        steers.push({ ...fields });
        await new Promise<void>((resolve, reject) => {
          const timer = window.setTimeout(() => { steerAck = null; publish(); reject(new Error("引导结果待确认")); }, 60000);
          steerAck = (accepted) => {
            window.clearTimeout(timer); steerAck = null;
            if (accepted) { deliveredSteers.push({ id: `steer-${steers.length}`, role: "user", text: fields.text, turnId: "fixture-live", turnRunning: true }); resolve(); }
            else reject(new Error("引导未交付：隔离拒绝"));
            publish();
          };
          publish();
        });
      }
      if (type === "configureSession") {
        checkFieldBounds(type, _fields);
        if (!workflowEnabled && (!permissions || detail().status !== "idle")) throw new Error("会话当前不可配置");
        const fields = _fields as CommandMap["configureSession"]["fields"];
        if (fields.sessionId !== "fixture-session" && !(workflowEnabled && fields.sessionId === "other-session")) throw new Error("会话不匹配");
        if (workflowEnabled && fields.model) workflowSelectModel(fields.model);
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
    const timer = window.setInterval(() => { tick++; counters.deliveries++; publish(); receive(workflowEnabled ? workflowDetail(_id) : detail()); }, 200);
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
  nextTurn: () => { if (folding) { foldingTurn++; turnEnded = false; activityComplete = false; waiting = false; } },
  updateUsage: () => { usageUpdated = true; },
  waitForApproval: () => { activityComplete = true; waiting = true; },
  failRead: () => { failRead = true; },
  holdRead: () => { holdRead = true; },
  releaseRead: () => releaseRead?.(),
  failMutation: () => { failMutation = true; },
  failWatch: () => (failWatch ?? noop)(),
  acceptSteer: () => steerAck?.(true),
  rejectSteer: () => steerAck?.(false),
  visibility(value: boolean) { hidden = value; document.dispatchEvent(new Event("visibilitychange")); publish(); },
  state: () => ({ ...counters, tick, hidden, held: releaseRead !== null }),
};
