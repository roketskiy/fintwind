/*
MIT License

Copyright (c) 2026 Igor Hipólito Vieira

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/
// Adapted from zignd/opencode-goal-plugin at cf07a69a4efb2c70a81c7da3dc17d9a9afdd0656.
// MIT, Copyright (c) 2026 Igor Hipólito Vieira. See opencode-goal-plugin.LICENSE.

import { watch } from "node:fs";
import { readFile } from "node:fs/promises";

type Status = "active" | "paused" | "done" | "blocked";
type Verdict = "done" | "continue" | "wait" | "blocked";
type Snapshot = { sessionId: string; objective: string; status: Status; turns: number; maxTurns: number; reason: string; revision: number; result?: string; model?: string; tools?: number; startedAt?: number; completedAt?: number; elapsedSeconds?: number; activeSince?: number };
type State = Snapshot & { lastEvent: string; lastText: string; stalled: number; repeats: number; attemptTurns: number; checkpointAt?: number };
type RecordState = { revision: number; goal: State | null };
type Settlement = { generation: number; verdict: Verdict; reason: string; summary?: string };
type Action = { type: "get" | "pause" | "resume" | "complete" | "clear" } | { type: "start"; objective: string };
type Registration = { dispose(): Promise<void> };
type Prompt = { sessionID: string; text: string; description: string; metadata: { fintwindGoal: true } };
type Input = Record<string, unknown>;
type ToolContext = { sessionID: string; signal: AbortSignal };
type Tool = {
  name: string;
  description: string;
  input: Input;
  options: { codemode: false };
  execute(input: Input, context: ToolContext): Promise<{ content: string }>;
};
type ToolEditor = {
  add(tool: Tool): void;
  list(): readonly { id: string }[];
  get(id: string): { id: string } | undefined;
  remove(id: string): void;
};
type SystemPart = { type: string; text: string };
type ContextEvent = { sessionID: string; system: SystemPart[]; tools: Record<string, unknown> };
type Context = {
  location: { directory: string };
  storage: { get(key: string): Promise<unknown>; set(key: string, value: unknown): Promise<void> };
  tool: {
    transform(edit: (editor: ToolEditor) => void): Promise<Registration>;
    list(): Promise<readonly { id: string }[]>;
  };
  session: {
    get(input: { sessionID: string }): Promise<unknown>;
    context(input: { sessionID: string }): Promise<readonly unknown[]>;
    prompt(input: { sessionID: string; id?: string; text: string; metadata: { fintwindGoal: true } }): Promise<unknown>;
    synthetic(input: Prompt): Promise<unknown>;
    hook(kind: "prompt", handler: (event: { sessionID: string; prompt: { text: string }; metadata?: Record<string, unknown> }) => Promise<void>): Promise<Registration>;
    hook(kind: "context", handler: (event: ContextEvent) => void): Promise<Registration>;
  };
  rpc: { register(definition: unknown, handlers: { control(input: unknown): Promise<Snapshot | null> }): Promise<Registration> };
  command: { transform(edit: (editor: { add(command: { name: string; description: string; execute(input: { sessionID: string; prompt: { text: string } }): Promise<void> }): void }) => void): Promise<Registration> };
  event: { subscribe(input: { signal: AbortSignal }): AsyncIterable<unknown> };
};

function object(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("Expected an object");
  return Object.fromEntries(Object.entries(value));
}
function modelKey(value: unknown): string | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  const model = Object.fromEntries(Object.entries(value));
  return typeof model.providerID === "string" && typeof model.id === "string" && model.providerID && model.id
    ? `${model.providerID}/${model.id}`
    : undefined;
}
function parseRecord(raw: unknown): RecordState {
  if (raw === undefined || raw === null) return { revision: 0, goal: null };
  const record = object(raw);
  if (!Number.isSafeInteger(record.revision) || typeof record.revision !== "number") throw new Error("Invalid goal revision");
  if (record.goal === null) return { revision: record.revision, goal: null };
  const s = object(record.goal);
  if (typeof s.sessionId !== "string" || typeof s.objective !== "string" || typeof s.reason !== "string" ||
      !["active", "paused", "done", "blocked"].includes(String(s.status)) ||
      typeof s.turns !== "number" || !Number.isSafeInteger(s.turns) || typeof s.maxTurns !== "number" || !Number.isSafeInteger(s.maxTurns) ||
      typeof s.lastEvent !== "string" || typeof s.lastText !== "string" || typeof s.stalled !== "number" || typeof s.repeats !== "number") throw new Error("Invalid saved goal");
  const status = s.status;
  if (status !== "active" && status !== "paused" && status !== "done" && status !== "blocked") throw new Error("Invalid goal status");
  return { revision: record.revision, goal: { sessionId: s.sessionId, objective: s.objective, reason: s.reason, status, turns: s.turns,
    maxTurns: s.maxTurns, attemptTurns: typeof s.attemptTurns === "number" && Number.isSafeInteger(s.attemptTurns) && s.attemptTurns >= 0 ? s.attemptTurns : s.turns, revision: record.revision, lastEvent: s.lastEvent, lastText: s.lastText, stalled: s.stalled, repeats: s.repeats,
    result: typeof s.result === "string" ? s.result : undefined, model: typeof s.model === "string" ? s.model : undefined,
    tools: typeof s.tools === "number" && Number.isSafeInteger(s.tools) ? s.tools : undefined,
    startedAt: typeof s.startedAt === "number" && Number.isSafeInteger(s.startedAt) ? s.startedAt : undefined,
    completedAt: typeof s.completedAt === "number" && Number.isSafeInteger(s.completedAt) ? s.completedAt : undefined,
    elapsedSeconds: typeof s.elapsedSeconds === "number" && Number.isSafeInteger(s.elapsedSeconds) && s.elapsedSeconds >= 0 ? s.elapsedSeconds : undefined,
    activeSince: typeof s.activeSince === "number" && Number.isSafeInteger(s.activeSince) && s.activeSince >= 0 ? s.activeSince : undefined,
    checkpointAt: typeof s.checkpointAt === "number" && Number.isSafeInteger(s.checkpointAt) && s.checkpointAt >= 0 ? s.checkpointAt : undefined } };
}
function parseAction(value: unknown): Action {
  const action = object(value);
  if (action.type === "start") {
    if (typeof action.objective !== "string" || !action.objective.trim() || action.objective.length > 16000) throw new Error("A goal must contain 1 to 16000 characters");
    return { type: "start", objective: action.objective.trim() };
  }
  const type = action.type;
  if (type === "get" || type === "pause" || type === "resume" || type === "complete" || type === "clear") return { type };
  throw new Error("Unknown goal action");
}
function view(state: State | null): Snapshot | null {
  if (!state) return null;
  const { sessionId, objective, status, turns, maxTurns, reason, revision, ...outcome } = state;
  return {
    sessionId, objective, status, turns, maxTurns, reason, revision,
    ...(typeof outcome.result === "string" ? { result: outcome.result } : {}),
    ...(typeof outcome.model === "string" ? { model: outcome.model } : {}),
    ...(typeof outcome.tools === "number" ? { tools: outcome.tools } : {}),
    ...(typeof outcome.startedAt === "number" ? { startedAt: outcome.startedAt } : {}),
    ...(typeof outcome.completedAt === "number" ? { completedAt: outcome.completedAt } : {}),
    ...(typeof outcome.elapsedSeconds === "number" ? { elapsedSeconds: outcome.elapsedSeconds } : {}),
    ...(typeof outcome.activeSince === "number" ? { activeSince: outcome.activeSince } : {}),
  };
}

const SETTLE_TOOL = "fintwind_goal_settle";

/** Per-request instruction for sessions with an active Goal. */
const GOAL_INSTRUCTION =
  "This session has an active Goal set by the user. Work toward it across turns until it is fully achieved.\n" +
  `- At the end of every turn in which you worked toward the Goal, call ${SETTLE_TOOL} exactly once, after all other work.\n` +
  '- Pass verdict "done" only when the objective is fully achieved and the evidence is in this session; "blocked" when you cannot proceed without external input or action; "continue" when more work is immediately actionable; "wait" when you have already scheduled a background task and should wait for its completion notification.\n' +
  "- Pass reason: one concise sentence justifying the verdict. When done, also pass summary: one concise sentence describing what was accomplished.\n" +
  "- After a done or blocked report the Goal stops; do not call the tool again.\n" +
  "- User messages during the Goal add guidance; follow them and keep working toward the Goal.";

/** The distinctive opening of {@link GOAL_INSTRUCTION}, used by the E2E. */
export const GOAL_INSTRUCTION_MARKER = "This session has an active Goal set by the user";

const schema = { type: "object", properties: { sessionID: { type: "string" }, inputID: { type: "string" }, action: { type: "object" } }, required: ["sessionID", "action"] };
const rpcDefinition = { id: "fintwind.goal", methods: { control: { input: schema, output: {} } }, events: {} };

export default {
  id: "fintwind.goal",
  async setup(ctx: Context) {
    type Slot = { generation: number; tail: Promise<unknown>; hydrated: boolean; settled: Settlement | undefined; reminded: boolean; children: Map<string, number>; windowID: string; windowTools: number };
    const slots = new Map<string, Slot>();
    // Sessions whose Goal is active right now. The context hook needs a
    // synchronous answer per model request, so state writes keep this set
    // current instead of touching storage on the request path.
    const driving = new Set<string>();
    const abort = new AbortController();
    const settingsPath = new URL("./settings.json", import.meta.url);
    let enabled = false;
    let settingsRevision: number | undefined;
    let command: Registration | undefined;
    let settingsTail: Promise<unknown> = Promise.resolve();
    const slotFor = (id: string) => {
      let slot = slots.get(id);
      if (!slot) { slot = { generation: 0, tail: Promise.resolve(), hydrated: false, settled: undefined, reminded: false, children: new Map(), windowID: "", windowTools: 0 }; slots.set(id, slot); }
      return slot;
    };
    // A generation bump discards a settlement reported by an older attempt and
    // grants one fresh missing-report reminder. Manual prompts, control
    // actions, disabling and disconnects all go through here.
    const invalidate = (slot: Slot) => {
      slot.generation++;
      slot.settled = undefined;
      slot.reminded = false;
    };
    const read = async (id: string): Promise<RecordState> => {
      const slot = slotFor(id);
      const record = parseRecord(await ctx.storage.get(`session:${id}`));
      if (!slot.hydrated) {
        slot.hydrated = true;
        if (record.goal?.status === "active") {
          const saved = record.goal;
          const elapsedSeconds = saved.elapsedSeconds === undefined ? undefined : saved.elapsedSeconds
            + (saved.activeSince === undefined ? 0 : Math.max(0, (saved.checkpointAt ?? saved.activeSince) - saved.activeSince));
          const goal = await write(id, record, { ...saved, status: "paused", elapsedSeconds, activeSince: undefined, reason: "Goal runtime restarted" });
          return { revision: record.revision + 1, goal };
        }
      }
      if (record.goal && record.goal.elapsedSeconds === undefined) {
        const goal = await write(id, record, record.goal);
        return { revision: record.revision + 1, goal };
      }
      return record;
    };
    const write = async (id: string, record: RecordState, goal: State | null) => {
      const revision = record.revision + 1;
      const now = Math.floor(Date.now() / 1000);
      let next: State | null = null;
      if (goal) {
        // Old records have no pause history. Preserve their known duration once.
        const elapsed = goal.elapsedSeconds ?? (goal.startedAt === undefined ? 0 : Math.max(0, (goal.completedAt ?? now) - goal.startedAt));
        const interval = goal.elapsedSeconds !== undefined && goal.activeSince !== undefined ? Math.max(0, now - goal.activeSince) : 0;
        next = { ...goal, revision, elapsedSeconds: elapsed + interval,
          activeSince: goal.status === "active" ? now : undefined,
          checkpointAt: goal.status === "active" ? now : undefined,
          completedAt: goal.status === "done" ? goal.completedAt ?? now : undefined };
      }
      await ctx.storage.set(`session:${id}`, { revision, goal: next });
      if (next?.status === "active") driving.add(id); else driving.delete(id);
      return next;
    };
    const serialize = <T>(slot: Slot, work: () => Promise<T>): Promise<T> => {
      const next = slot.tail.then(work, work);
      slot.tail = next.catch(() => {});
      return next;
    };
    const owned = async (id: string) => {
      const session = object(await ctx.session.get({ sessionID: id }));
      const location = object(session.location);
      if (location.directory !== ctx.location.directory) throw new Error("Goal session belongs to another location");
    };
    const send = async (id: string, slot: Slot, generation: number, state: State, kind: "initial" | "continue" | "remind", inputID?: string) => {
      await refreshSettings();
      if (!enabled || abort.signal.aborted || generation !== slot.generation) return;
      if (kind === "initial") {
        await ctx.session.prompt({ sessionID: id, id: inputID, text: state.objective, metadata: { fintwindGoal: true } });
        return;
      }
      if (kind === "remind") {
        await ctx.session.synthetic({
          sessionID: id, description: "Goal settlement reminder", metadata: { fintwindGoal: true },
          text: `This session has an active Goal: ${state.objective}\n\nYour last turn ended without calling the ${SETTLE_TOOL} tool. Call it now with verdict, reason and summary for the work you just completed.`,
        });
        return;
      }
      await ctx.session.synthetic({ sessionID: id, description: "Goal continuation", metadata: { fintwindGoal: true }, text: `Continue working toward this goal: ${state.objective}\n\n${state.reason || "Work until the outcome is achieved. Verify the result and report concrete evidence."}` });
    };
    // Progress evidence of the latest attempt. The settlement call itself is
    // bookkeeping, not work, so it never counts as a tool call here. A turn
    // window opens at the objective prompt and at the plugin's own synthetic
    // continuations and reminders. Foreign synthetic injections (background
    // task notifications) must not split the window: work done before a
    // notification still belongs to this attempt, and a wake-up that only
    // delivers a notification is not evidence by itself.
    const summarize = async (id: string) => {
      const messages = await ctx.session.context({ sessionID: id });
      let text = ""; let tools = 0; let model: unknown; let windowID = "";
      for (const rawMessage of messages) {
        const message = object(rawMessage);
        if (message.type === "user") { text = ""; tools = 0; windowID = typeof message.id === "string" ? message.id : windowID; continue; }
        if (message.type === "synthetic") {
          const rawMetadata = message.metadata;
          const own = typeof rawMetadata === "object" && rawMetadata !== null && !Array.isArray(rawMetadata)
            && object(rawMetadata).fintwindGoal === true;
          if (!own) continue;
          text = ""; tools = 0; windowID = typeof message.id === "string" ? message.id : windowID; continue;
        }
        if (message.type !== "assistant") continue;
        model = message.model ?? model;
        if (!Array.isArray(message.content)) continue;
        for (const rawPart of message.content) {
          const part = object(rawPart);
          if (part.type === "text" && typeof part.text === "string") text += part.text + "\n";
          if (part.type === "tool" && part.name !== SETTLE_TOOL) tools++;
        }
      }
      return { text: text.trim().slice(-24000), tools, model: modelKey(model), windowID };
    };
    // Evidence that a "wait" settlement cites real background work: a child
    // session whose completion notification appeared since the previous
    // check, or a known child session that produced fresh tool calls. Only
    // the notification metadata (source "subagent", childID) is read; the
    // notification text is never parsed. The cumulative per-child tool counts
    // live in the slot, so they reset whenever the attempt restarts.
    const childProgress = async (id: string, slot: Slot) => {
      let messages: readonly unknown[];
      try { messages = await ctx.session.context({ sessionID: id }); } catch { return false; }
      let progress = false;
      for (const rawMessage of messages) {
        const message = object(rawMessage);
        if (message.type !== "synthetic") continue;
        const rawMetadata = message.metadata;
        if (typeof rawMetadata !== "object" || rawMetadata === null || Array.isArray(rawMetadata)) continue;
        const metadata = object(rawMetadata);
        if (metadata.source !== "subagent" || typeof metadata.childID !== "string" || !metadata.childID) continue;
        const childID = metadata.childID;
        if (!slot.children.has(childID)) {
          // First sight of a completed child: its work happened since the
          // last settlement, so it counts as progress on its own.
          let tools = 0;
          try {
            const child = await ctx.session.context({ sessionID: childID });
            for (const rawChildMessage of child) {
              const childMessage = object(rawChildMessage);
              if (childMessage.type !== "assistant" || !Array.isArray(childMessage.content)) continue;
              for (const rawPart of childMessage.content) {
                if (object(rawPart).type === "tool") tools++;
              }
            }
          } catch { /* a child we cannot read still proved its completion */ }
          slot.children.set(childID, tools);
          progress = true;
          continue;
        }
        if (progress) continue;
        try {
          const child = await ctx.session.context({ sessionID: childID });
          let tools = 0;
          for (const rawChildMessage of child) {
            const childMessage = object(rawChildMessage);
            if (childMessage.type !== "assistant" || !Array.isArray(childMessage.content)) continue;
            for (const rawPart of childMessage.content) {
              if (object(rawPart).type === "tool") tools++;
            }
          }
          if (tools > (slot.children.get(childID) ?? 0)) progress = true;
          slot.children.set(childID, tools);
        } catch { /* keep the last known count */ }
      }
      return progress;
    };
    const settleTool: Tool = {
      name: SETTLE_TOOL,
      description: "Report this active Goal turn's outcome. " +
        "Call it exactly once at the end of every turn in which you worked toward the Goal, after all other work is finished. " +
        'Use "continue" for immediately actionable work, or "wait" after scheduling a background task whose completion notification should resume the session. "done" means the Goal is fully achieved. ' +
        "Only for an active Goal; never for ordinary follow-ups or historical Goals.",
      options: { codemode: false },
      input: {
        type: "object",
        properties: {
          verdict: { type: "string", enum: ["done", "continue", "wait", "blocked"] },
          reason: { type: "string", minLength: 1, maxLength: 2000 },
          summary: { type: "string", minLength: 1, maxLength: 2000 },
        },
        required: ["verdict", "reason"],
        additionalProperties: false,
      },
      async execute(raw, context) {
        const input = object(raw);
        const verdict = input.verdict;
        if (verdict !== "done" && verdict !== "continue" && verdict !== "wait" && verdict !== "blocked") throw new Error('verdict must be "done", "continue", "wait" or "blocked". No settlement was recorded.');
        const reason = typeof input.reason === "string" ? input.reason.trim().slice(0, 2000) : "";
        if (!reason) throw new Error("reason must be a non-empty sentence. No settlement was recorded.");
        const summary = typeof input.summary === "string" && input.summary.trim() ? input.summary.trim().slice(0, 2000) : undefined;
        const slot = slots.get(context.sessionID);
        if (!slot) throw new Error("No active Goal.");
        await serialize(slot, async () => {
          const record = await read(context.sessionID);
          if (!enabled || context.signal.aborted || !record.goal || record.goal.status !== "active") {
            throw new Error("Goal settlement is unavailable for this request.");
          }
          if (slot.settled?.generation === slot.generation) {
            throw new Error("This turn already has a Goal outcome.");
          }
          slot.settled = { generation: slot.generation, verdict, reason, summary };
        });
        return { content: verdict === "done" ? "Goal completed." : verdict === "blocked" ? "Goal blocked." : "Goal outcome recorded." };
      },
    };
    const control = async (raw: unknown): Promise<Snapshot | null> => {
      const input = object(raw);
      if (typeof input.sessionID !== "string" || !input.sessionID) throw new Error("Missing sessionID");
      if (input.inputID !== undefined && (typeof input.inputID !== "string" || !input.inputID)) throw new Error("Invalid inputID");
      const inputID = typeof input.inputID === "string" ? input.inputID : undefined;
      const id = input.sessionID;
      const action = parseAction(input.action);
      if (action.type === "start" || action.type === "resume") {
        await refreshSettings();
        if (!enabled) throw new Error("Enable Goal in settings first");
      }
      const slot = slotFor(id);
      if (action.type !== "get" && action.type !== "resume") invalidate(slot);
      let generation = slot.generation;
      let shouldSend = action.type === "start";
      const state = await serialize(slot, async () => {
        await owned(id);
        const record = await read(id);
        if ((action.type === "start" || action.type === "resume") && !enabled) throw new Error("Enable Goal in settings first");
        if (action.type === "get") {
          // Checkpoint without changing the public snapshot or revision: polling must not trigger UI rebuilds.
          const goal = record.goal;
          const now = Math.floor(Date.now() / 1000);
          if (goal?.status === "active" && goal.checkpointAt !== now) {
            await ctx.storage.set(`session:${id}`, { revision: record.revision, goal: { ...goal, checkpointAt: now } });
          }
          return goal;
        }
        if (action.type === "clear") return write(id, record, null);
        if (action.type === "start") { slot.children.clear(); slot.windowID = ""; slot.windowTools = 0; return write(id, record, { sessionId: id, objective: action.objective, status: "active", turns: 0, attemptTurns: 0, maxTurns: 20,
          reason: "", revision: 0, lastEvent: "", lastText: "", stalled: 0, repeats: 0, result: undefined, model: undefined, tools: undefined,
          startedAt: Math.floor(Date.now() / 1000), elapsedSeconds: 0, activeSince: Math.floor(Date.now() / 1000), completedAt: undefined }); }
        if (!record.goal) { if (action.type === "pause") return null; throw new Error("No goal in this session"); }
        if (action.type === "pause") return record.goal.status === "active" ? write(id, record, { ...record.goal, status: "paused", reason: "Paused by the user" }) : record.goal;
        if (action.type === "complete") return write(id, record, { ...record.goal, status: "done", reason: "Completed by the user" });
        if (record.goal.status === "active") return record.goal;
        invalidate(slot);
        generation = slot.generation;
        shouldSend = true;
        slot.children.clear();
        slot.windowID = "";
        slot.windowTools = 0;
        return write(id, record, { ...record.goal, status: "active", attemptTurns: 0, stalled: 0, repeats: 0, reason: "", lastText: "", result: undefined,
          startedAt: record.goal.startedAt ?? Math.floor(Date.now() / 1000), completedAt: undefined });
      });
      if (shouldSend && state) {
        try { await send(id, slot, generation, state, "initial", inputID); }
        catch (error) {
          await serialize(slot, async () => {
            const record = await read(id);
            if (slot.generation === generation && record.goal) await write(id, record, { ...record.goal, status: "paused", reason: String(error) });
          });
          throw error;
        }
      }
      return view(state);
    };
    const promptHook = await ctx.session.hook("prompt", async event => {
      if (event.metadata?.fintwindGoal !== true && !/^\s*\/goal(?:\s|$)/i.test(event.prompt.text)) {
        invalidate(slotFor(event.sessionID));
      }
    });
    const toolRegistration = await ctx.tool.transform(editor => editor.add(settleTool));
    const registered = await ctx.tool.list();
    if (!registered.some(entry => entry.id === SETTLE_TOOL)) throw new Error("OpenCode did not register the Fintwind goal settlement tool.");
    const contextHook = await ctx.session.hook("context", event => {
      if (!enabled || typeof event.sessionID !== "string" || !driving.has(event.sessionID)) {
        delete event.tools[SETTLE_TOOL];
        return;
      }
      event.system.push({ type: "text", text: GOAL_INSTRUCTION });
    });
    const rpc = await ctx.rpc.register(rpcDefinition, { control });
    const registerCommand = () => ctx.command.transform(editor => editor.add({
      name: "goal", description: "Set a persistent goal. /goal <objective>, status, pause, resume, complete, clear",
      execute: async ({ sessionID, prompt }) => {
        const text = prompt.text.replace(/^\s*\/goal\b/i, "").trim();
        let action: Action;
        if (!text || text === "status") action = { type: "get" };
        else if (text === "pause" || text === "resume" || text === "clear" || text === "complete") action = { type: text };
        else action = { type: "start", objective: text };
        await control({ sessionID, action });
      },
    }));
    const applySettings = async (next: boolean, revision?: number) => {
      if (abort.signal.aborted) return;
      const missedChange = enabled && next && revision !== settingsRevision;
      settingsRevision = revision;
      if (enabled === next && !missedChange) return;
      enabled = next;
      if (!next || missedChange) {
        await Promise.all([...slots].map(async ([id, slot]) => {
          invalidate(slot);
          await serialize(slot, async () => {
            const record = await read(id);
            if (record.goal?.status === "active") await write(id, record, { ...record.goal, status: "paused", reason: "Goal disabled in settings" });
          }).catch(() => {});
        }));
      }
      if (enabled && !command) command = await registerCommand();
      else if (!enabled && command) { await command.dispose(); command = undefined; }
    };
    const refreshSettings = (): Promise<void> => {
      const refresh = async () => {
        try {
          const settings = object(JSON.parse(await readFile(settingsPath, "utf8")));
          const revision = typeof settings.revision === "number" && Number.isSafeInteger(settings.revision) ? settings.revision : undefined;
          await applySettings(settings.enabled === true, revision);
        } catch { await applySettings(false); }
      };
      const next = settingsTail.then(refresh, refresh);
      settingsTail = next.catch(() => {});
      return next;
    };
    const settle = async (raw: unknown) => {
      const event = object(raw);
      if (event.type !== "session.execution.succeeded" && event.type !== "session.execution.failed" && event.type !== "session.execution.interrupted") return;
      const data = object(event.data);
      if (typeof data.sessionID !== "string" || typeof event.id !== "string") return;
      const id = data.sessionID;
      if (event.location !== undefined) {
        if (object(event.location).directory !== ctx.location.directory) return;
      } else await owned(id);
      await refreshSettings();
      if (!enabled) return;
      const slot = slots.get(id);
      if (!slot) return;
      let followup: { kind: "continue" | "remind"; generation: number; state: State } | undefined;
      let report: Settlement | undefined;
      await serialize(slot, async () => {
        const record = await read(id);
        if (!record.goal || record.goal.status !== "active" || record.goal.lastEvent === String(event.id)) return;
        const generation = slot.generation;
        let next: State = { ...record.goal, lastEvent: String(event.id), turns: record.goal.turns + 1, attemptTurns: record.goal.attemptTurns + 1 };
        if (event.type !== "session.execution.succeeded") {
          next = { ...next, status: "paused", reason: event.type === "session.execution.failed" ? "The turn failed" : "The turn was interrupted" };
        } else {
          const scan = await summarize(id);
          next.stalled = scan.tools === 0 ? next.stalled + 1 : 0;
          next.repeats = scan.text && scan.text === next.lastText ? next.repeats + 1 : 0;
          next.lastText = scan.text;
          next.model = scan.model;
          // The card reports the Goal's whole effort, so tool calls add up
          // across attempts instead of describing only the final one. When a
          // foreign notification extended the same window, only the growth
          // since the previous settlement is new evidence.
          const sameWindow = scan.windowID !== "" && scan.windowID === slot.windowID;
          next.tools = (sameWindow ? Math.max(scan.tools - slot.windowTools, 0) : scan.tools) + (next.tools ?? 0);
          slot.windowID = scan.windowID;
          slot.windowTools = scan.tools;
          next.result = undefined;
          report = slot.settled && slot.settled.generation === generation ? slot.settled : undefined;
          slot.settled = undefined;
          if (report) {
            slot.reminded = false;
            next.reason = report.reason;
            if (report.verdict === "done") {
              next.status = "done";
              next.result = report.summary;
            } else if (report.verdict === "blocked") next.status = "blocked";
          } else if (slot.reminded) {
            next.status = "paused";
            next.reason = "No settlement was reported";
          } else {
            slot.reminded = true;
          }
          // A "wait" verdict only suspends the stall guard when the
          // background work it cites is real: without fresh child evidence a
          // settle-only waiter accumulates stalls like any other idle loop.
          if (report?.verdict === "wait" && next.stalled > 0 && await childProgress(id, slot)) {
            next.stalled = 0;
            next.repeats = 0;
          }
        }
        if (next.status === "active") {
          if (next.attemptTurns >= next.maxTurns) { next.status = "paused"; next.reason = "Turn limit reached"; }
          else if (next.stalled >= 3 || next.repeats >= 2) { next.status = "blocked"; next.reason = "No further progress detected"; }
        }
        const saved = await write(id, record, next);
        if (saved?.status === "active" && report?.verdict !== "wait") followup = { kind: report ? "continue" : "remind", generation, state: saved };
      });
      if (followup) await send(id, slot, followup.generation, followup.state, followup.kind);
    };
    const watcher = watch(new URL("./", import.meta.url), () => {
      void refreshSettings().catch(() => {});
    });
    watcher.on("error", () => {
      const next = settingsTail.then(() => applySettings(false), () => applySettings(false));
      settingsTail = next.catch(() => {});
    });
    await refreshSettings();
    void (async () => {
      while (!abort.signal.aborted) {
        try {
          for await (const event of ctx.event.subscribe({ signal: abort.signal })) {
            if (abort.signal.aborted) break;
            void settle(event).catch(() => {});
          }
        } catch {}
        if (!abort.signal.aborted) {
          await Promise.all([...slots].map(async ([id, slot]) => {
            invalidate(slot);
            await serialize(slot, async () => {
              const record = await read(id);
              if (record.goal?.status === "active") await write(id, record, { ...record.goal, status: "paused", reason: "Goal event stream disconnected" });
            }).catch(() => {});
          }));
          await new Promise(resolve => setTimeout(resolve, 1000));
        }
      }
    })();
    return async () => {
      abort.abort();
      watcher.close();
      for (const slot of slots.values()) invalidate(slot);
      driving.clear();
      await settingsTail;
      await toolRegistration.dispose().catch(() => {});
      await command?.dispose();
      await promptHook.dispose();
      await contextHook.dispose();
      await rpc.dispose();
    };
  },
};
