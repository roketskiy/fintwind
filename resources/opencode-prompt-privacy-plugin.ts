import { createHash, createHmac, randomBytes } from "node:crypto";
import { link, mkdir, readFile, unlink, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { discover, fieldName, isFieldValue } from "./prompt-privacy-detect.ts";

type Registration = { dispose(): Promise<void> };
type HttpEvent = { sessionID: string; request: Request };
type HttpResponseEvent = { sessionID: string; request: Request; response: Response };
type FrameEvent = { sessionID: string; frame: string };
type ShellEvent = { command: string; env: Record<string, string | undefined> };
type Context = {
  rpc: { register(definition: unknown, handlers: { resolve(input: unknown): Promise<unknown> }): Promise<Registration> };
  tool: { hook(name: "execute.before", callback: (event: ToolEvent) => Promise<void>): Promise<Registration> };
  session: { hook<T>(name: string, callback: (event: T) => void | Promise<void>): Promise<Registration> };
  shell: { hook(name: "create.before", callback: (event: ShellEvent) => Promise<void>): Promise<Registration> };
};

const referenceSource = "FINTWIND_SECRET_[A-F0-9]{24}";
const referencePrefix = referenceSource.slice(0, referenceSource.indexOf("["));
const embeddedReference = new RegExp(referenceSource, "g");
const reference = new RegExp(`\\b${referenceSource}\\b`, "g");

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
type FieldScope = "request" | "content" | "text" | "chat-tool-call" | "function" |
  "function-response" | "tool-data" | "encoded-tool-data" | "tool-definition" | "schema-properties" | "format";

function childScope(parent: { [key: string]: Json }, key: string, scope: FieldScope): FieldScope | undefined {
  switch (scope) {
    case "request":
      if (key === "instructions") return "text";
      if (key === "tools") return "tool-definition";
      if (key === "text" || key === "response_format" || key === "generationConfig") return "format";
      if (key === "input" || key === "messages" || key === "system" || key === "contents"
        || key === "systemInstruction" || key === "system_instruction") return "content";
      break;
    case "content":
      if (key === "parts") return "content";
      if (key === "content" && (typeof parent.role === "string" || parent.type === "tool_result"
        || parent.type === "mcp_tool_result" || parent.type === "reasoning")) return "content";
      if (key === "summary" && parent.type === "reasoning") return "content";
      if (key === "text" && (parent.type === undefined || parent.type === "text" || parent.type === "input_text"
        || parent.type === "output_text" || parent.type === "summary_text" || parent.type === "reasoning_text")) return "text";
      if (key === "refusal" && (parent.type === "refusal" || parent.role === "assistant")) return "text";
      if (key === "reasoning_content" && parent.role === "assistant") return "text";
      if (key === "tool_calls" && parent.role === "assistant") return "chat-tool-call";
      if ((key === "function_call" && parent.role === "assistant") || key === "functionCall") return "function";
      if (key === "functionResponse") return "function-response";
      if (key === "arguments" && (parent.type === "function_call" || parent.type === "mcp_call")) return "encoded-tool-data";
      if (key === "input" && (parent.type === "tool_use" || parent.type === "server_tool_use"
        || parent.type === "custom_tool_call" || parent.type === "mcp_tool_use")) return "tool-data";
      if (key === "output" && (parent.type === "function_call_output" || parent.type === "custom_tool_call_output"
        || parent.type === "mcp_call")) return typeof parent.output === "string" ? "encoded-tool-data" : "content";
      break;
    case "chat-tool-call":
      if (key === "function") return "function";
      if (key === "custom") return "function";
      break;
    case "function":
      if (key === "arguments") return "encoded-tool-data";
      if (key === "args" || key === "input") return "tool-data";
      break;
    case "function-response":
      if (key === "response") return "tool-data";
      if (key === "parts") return "content";
      break;
    case "tool-data":
      return "tool-data";
    case "tool-definition":
      if (key === "description" || key === "title") return "text";
      if (key === "default" || key === "examples" || key === "example") return "tool-data";
      if (key === "properties" || key === "$defs" || key === "definitions") return "schema-properties";
      if (["function", "parameters", "input_schema", "inputSchema", "items", "anyOf", "allOf", "oneOf",
        "not", "additionalProperties", "functionDeclarations"].includes(key)) return "tool-definition";
      break;
    case "schema-properties":
      return "tool-definition";
    case "format":
      if (key === "format" || key === "json_schema" || key === "responseSchema" || key === "responseJsonSchema") return "format";
      if (key === "schema") return "tool-definition";
      if (key === "properties" || key === "$defs" || key === "definitions") return "schema-properties";
      if (key === "description" || key === "title") return "text";
      if (key === "default" || key === "example" || key === "examples") return "tool-data";
      if (["items", "anyOf", "allOf", "oneOf", "not", "additionalProperties"].includes(key)) return "tool-definition";
      break;
    case "text":
    case "encoded-tool-data":
      break;
  }
}

function walk(
  value: Json,
  visit: (text: string, key: string) => string,
  key = "",
  scope: FieldScope = "request",
): Json {
  if (typeof value === "string") {
    if (scope !== "text" && scope !== "content" && scope !== "tool-data" && scope !== "encoded-tool-data") return value;
    if ((scope === "encoded-tool-data" || scope === "tool-data") && /^\s*[\[{]/.test(value)) {
      let nested: Json;
      try { nested = JSON.parse(value); } catch { return visit(value, key); }
      const next = walk(nested, visit, "", "tool-data");
      return next === nested ? value : JSON.stringify(next);
    }
    return visit(value, key);
  }
  if (!value || typeof value !== "object") return value;
  if (Array.isArray(value)) {
    const next = value.map(item => walk(item, visit, key, scope));
    return next.every((item, index) => item === value[index]) ? value : next;
  }
  const entries = Object.entries(value);
  const next = entries.map(([name, child]) => {
    const nextScope = childScope(value, name, scope);
    return [name, nextScope ? walk(child, visit, name, nextScope) : child] as const;
  });
  return next.every((entry, index) => entry[1] === entries[index][1]) ? value : Object.fromEntries(next);
}

function escapeRegex(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function splitReferences(text: string, resolve: (ref: string) => string | undefined,
  encoded: boolean): [string, string] {
  let output = "";
  for (let index = 0; index < text.length;) {
    const next = text.indexOf(referencePrefix[0], index);
    if (next === -1) return [output + text.slice(index), ""];
    output += text.slice(index, next);
    index = next;
    const remaining = text.slice(index);
    if (remaining.length < referencePrefix.length && referencePrefix.startsWith(remaining)) return [output, remaining];
    if (remaining.startsWith(referencePrefix)) {
      const suffix = remaining.slice(referencePrefix.length);
      if (suffix.length < 24 && /^[A-F0-9]*$/.test(suffix)) return [output, remaining];
      if (/^[A-F0-9]{24}/.test(suffix)) {
        const ref = remaining.slice(0, referencePrefix.length + 24);
        const original = resolve(ref);
        if (original === undefined) throw new Error("Private-value reference is unavailable");
        if (encoded && JSON.stringify(original).slice(1, -1) !== original) {
          throw new Error("Cannot safely restore escaped private value in streamed tool arguments");
        }
        output += encoded ? JSON.stringify(original).slice(1, -1) : original;
        index += ref.length;
        continue;
      }
    }
    output += text[index++];
  }
  return [output, ""];
}

type SessionPrivacy = { refs: Map<string, string>; scanned: Set<string>; matcher?: RegExp; continuationBlocked?: boolean };
type ProcessPrivacy = { seed: Buffer; originalByRef: Map<string, string>; sessions: Map<string, SessionPrivacy>;
  signedThinking: Map<string, string> };
const privacyKey = Symbol.for("fintwind.prompt-privacy.process-state.v2");
const processGlobals = globalThis as typeof globalThis & { [privacyKey]?: ProcessPrivacy };

async function processPrivacy(keyPath: string): Promise<ProcessPrivacy> {
  // A location plugin can unload between turns. Keep the references in this
  // OpenCode process without storing originals on disk or in its environment.
  if (!processGlobals[privacyKey]) {
    const seed = await privacySeed(keyPath);
    if (processGlobals[privacyKey]) return processGlobals[privacyKey]!;
    const state: ProcessPrivacy = { seed, originalByRef: new Map(), sessions: new Map(), signedThinking: new Map() };
    Object.defineProperty(processGlobals, privacyKey, { value: state, enumerable: false });
  }
  return processGlobals[privacyKey]!;
}

async function privacySeed(path: string): Promise<Buffer> {
  try {
    const seed = await readFile(path);
    if (seed.length !== 32) throw new Error("Invalid prompt privacy key");
    return seed;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }
  await mkdir(dirname(path), { recursive: true });
  const temporary = `${path}.${randomBytes(16).toString("hex")}.tmp`;
  try {
    await writeFile(temporary, randomBytes(32), { flag: "wx", mode: 0o600 });
    try { await link(temporary, path); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
    }
    const seed = await readFile(path);
    if (seed.length !== 32) throw new Error("Invalid prompt privacy key");
    return seed;
  } finally {
    await unlink(temporary).catch(() => {});
  }
}

type ToolEvent = { sessionID: string; tool: string; input: unknown };

function restoreToolInput(input: unknown, resolve: (ref: string) => string): unknown {
  if (typeof input === "string") return input.replace(reference, resolve);
  if (Array.isArray(input)) return input.map(item => restoreToolInput(item, resolve));
  if (input && typeof input === "object") {
    return Object.fromEntries(Object.entries(input).map(([key, item]) => [key, restoreToolInput(item, resolve)]));
  }
  return input;
}

const outputRpc = {
  id: "fintwind.privacy",
  methods: {
    resolve: {
      input: {
        type: "object", required: ["sessionID", "references"], additionalProperties: false,
        properties: {
          sessionID: { type: "string" },
          references: { type: "array", items: { type: "string", pattern: `^${referenceSource}$` } },
        },
      },
      output: { type: "object", additionalProperties: { type: "string" } },
    },
  },
  events: {},
};

export default {
  id: "fintwind.prompt-privacy",
  async setup(ctx: Context) {
    const settingsPath = process.env.FINTWIND_PROMPT_PRIVACY_SETTINGS;
    const keyPath = process.env.FINTWIND_PROMPT_PRIVACY_KEY_PATH
      ?? join(settingsPath ? dirname(settingsPath) : join(homedir(), ".fintwind"), "prompt-privacy.key");
    const { seed, originalByRef, sessions, signedThinking } = await processPrivacy(keyPath);
    const registrations: Registration[] = [];
    const httpEnabled = new WeakMap<Request, boolean>();
    const verifiedHistory = new Set<string>();
    const historyChecks = new Map<string, Promise<void>>();

    async function verifyHistory(sessionID: string): Promise<void> {
      if (verifiedHistory.has(sessionID)) return;
      let check = historyChecks.get(sessionID);
      if (!check) {
        check = (async () => {
          const port = process.env.FINTWIND_OPENCODE_SERVER_PORT;
          const password = process.env.OPENCODE_SERVER_PASSWORD;
          if (!port || !password) throw new Error("Cannot safely read local privacy history");
          const authorization = `Basic ${Buffer.from(`${process.env.OPENCODE_SERVER_USERNAME ?? "opencode"}:${password}`).toString("base64")}`;
          let cursor: string | undefined;
          const seen = new Set<string>();
          do {
            const url = new URL(`/api/session/${encodeURIComponent(sessionID)}/message`, `http://127.0.0.1:${port}`);
            url.searchParams.set("limit", "200");
            if (cursor) url.searchParams.set("cursor", cursor);
            else url.searchParams.set("order", "asc");
            const response = await fetch(url, { headers: { authorization }, signal: AbortSignal.timeout(15_000), proxy: "" });
            if (!response.ok) throw new Error("Cannot safely read local privacy history");
            const page = await response.json() as { data?: Json[]; cursor?: { next?: string | null } };
            if (!Array.isArray(page.data) || !page.cursor || !("next" in page.cursor)) {
              throw new Error("Cannot safely read local privacy history");
            }
            for (const message of page.data) {
              if (!message || Array.isArray(message) || typeof message !== "object") continue;
              for (const key of ["text", "content", "summary", "command", "input", "output", "result"]) {
                if (message[key] !== undefined) scanPrivacy(message[key], sessionID, "tool-data", false);
              }
            }
            if (page.cursor.next != null && typeof page.cursor.next !== "string") throw new Error("Cannot safely read local privacy history");
            if (!page.data.length && page.cursor.next) throw new Error("Cannot safely read local privacy history");
            cursor = page.cursor.next ?? undefined;
            if (cursor && seen.has(cursor)) throw new Error("Cannot safely read local privacy history");
            if (cursor) seen.add(cursor);
          } while (cursor);
          const state = sessions.get(sessionID);
          if (state) buildMatcher(state);
          verifiedHistory.add(sessionID);
        })();
        historyChecks.set(sessionID, check);
      }
      try { await check; } finally { historyChecks.delete(sessionID); }
    }

    async function enabled(): Promise<boolean> {
      if (!settingsPath) return true;
      try {
        return JSON.parse(await readFile(settingsPath, "utf8")).prompt_privacy_enabled !== false;
      } catch {
        return true;
      }
    }

    function buildMatcher(state: SessionPrivacy): void {
      if (!state.refs.size) return;
      const wordCharacter = "[\\p{L}\\p{M}\\p{N}_]";
      const patterns = [...state.refs.keys()].sort((first, second) => second.length - first.length).map(value => {
        const pattern = escapeRegex(value);
        return value.length < 8 ? `(?<!${wordCharacter})${pattern}(?!${wordCharacter})` : pattern;
      });
      state.matcher = new RegExp(`${referenceSource}|${patterns.join("|")}`, "gu");
    }

    function scanPrivacy(input: Json, sessionID: string, scope: FieldScope = "request", rebuild = true): void {
      let state = sessions.get(sessionID);
      if (!state) {
        state = { refs: new Map(), scanned: new Set(), continuationBlocked: true };
        sessions.set(sessionID, state);
      }
      const { refs, scanned } = state;
      const oldSize = refs.size;
      function remember(spelling: string, original = spelling) {
        let ref = refs.get(original);
        if (!ref) {
          const suffix = createHmac("sha256", seed).update(original)
            .digest("hex").slice(0, 24).toUpperCase();
          ref = `FINTWIND_SECRET_${suffix}`;
          refs.set(original, ref);
          originalByRef.set(ref, original);
        }
        refs.set(spelling, ref);
      }
      walk(input, (text, key) => {
        if (fieldName.test(key) && isFieldValue(text, key)) remember(text);
        const fingerprint = createHash("sha256").update(text).digest("base64");
        if (!scanned.has(fingerprint)) {
          for (const { spelling, original } of discover(text)) remember(spelling, original);
          if (scanned.size >= 4096) scanned.clear();
          scanned.add(fingerprint);
        }
        return text;
      }, "", scope);
      if (rebuild && oldSize !== refs.size) buildMatcher(state);
    }

    function redact(body: string, sessionID: string, continuation = false): string {
      const input: Json = JSON.parse(body);
      if (!input || Array.isArray(input) || typeof input !== "object"
        || !("input" in input || "messages" in input || "contents" in input)) {
        throw new Error("Unsupported request shape for prompt privacy");
      }
      let signedChanged = false;
      const oldSize = sessions.get(sessionID)?.refs.size ?? 0;
      scanPrivacy(input, sessionID);
      const state = sessions.get(sessionID)!;
      const { refs } = state;
      let unavailable = false;
      walk(input, text => {
        for (const match of text.matchAll(reference)) {
          const original = originalByRef.get(match[0]);
          if (original === undefined || refs.get(original) !== match[0]) {
            unavailable = true;
          }
        }
        return text;
      });
      if (continuation && (oldSize !== refs.size || state.continuationBlocked)) {
        state.continuationBlocked = true;
        throw new Error("WebSocket continuation blocked because privacy detection changed");
      }
      if (unavailable) throw new Error("Private-value reference is unavailable");
      // Tool names are protocol identifiers, not content. Replacing them would
      // break dispatch, but forwarding a detected value there would expose it.
      function checkToolNames(value: Json): void {
        if (!value || typeof value !== "object") return;
        if (Array.isArray(value)) { value.forEach(checkToolNames); return; }
        const check = (name: Json | undefined) => {
          if (typeof name !== "string") return;
          let known = false;
          if (state.matcher) name.replace(state.matcher, value => {
            if (refs.has(value)) known = true;
            return value;
          });
          if (known || !discover(name)[Symbol.iterator]().next().done) {
            throw new Error("Cannot preserve tool structure while protecting prompt privacy");
          }
        };
        if (["function_call", "custom_tool_call", "tool_use", "server_tool_use", "mcp_tool_use", "function"].includes(String(value.type))
          || "parameters" in value || "input_schema" in value) check(value.name);
        for (const key of ["function", "functionCall", "functionResponse"]) {
          const candidate = value[key];
          if (candidate && !Array.isArray(candidate) && typeof candidate === "object") check(candidate.name);
        }
        Object.values(value).forEach(checkToolNames);
      }
      checkToolNames(input);
      // A signed Anthropic thinking block cannot be changed without invalidating
      // its signature. Never forward a detected original from such a block.
      if (input && !Array.isArray(input) && typeof input === "object" && Array.isArray(input.messages)) {
        for (const message of input.messages) {
          if (!message || Array.isArray(message) || typeof message !== "object" || message.role !== "assistant"
            || !Array.isArray(message.content)) continue;
          for (const block of message.content) {
            if (!block || Array.isArray(block) || typeof block !== "object" || block.type !== "thinking"
              || typeof block.thinking !== "string" || typeof block.signature !== "string") continue;
            const original = signedThinking.get(block.signature);
            if (original !== undefined && restoreText(original, sessionID) === block.thinking) {
              let leaked = false;
              if (state.matcher) original.replace(state.matcher, value => {
                if (refs.has(value)) leaked = true;
                return value;
              });
              if (leaked || !discover(original)[Symbol.iterator]().next().done) {
                throw new Error("Cannot safely send signed thinking containing a private value");
              }
              block.thinking = original;
              signedChanged = true;
              continue;
            }
            throw new Error("Cannot safely send signed thinking without its original content");
          }
        }
      }
      if (!continuation) state.continuationBlocked = false;
      if (!state.matcher) return signedChanged ? JSON.stringify(input) : body;
      const next = walk(input, text => text.replace(state.matcher!, value => refs.get(value) ?? value));
      return next === input && !signedChanged ? body : JSON.stringify(next);
    }

    function resolve(sessionID: string, ref: string): string | undefined {
      const original = originalByRef.get(ref);
      return original !== undefined && sessions.get(sessionID)?.refs.get(original) === ref ? original : undefined;
    }

    function restoreText(text: string, sessionID: string, encoded = false): string {
      return text.replace(embeddedReference, ref => {
        const original = resolve(sessionID, ref);
        if (original === undefined) throw new Error("Private-value reference is unavailable");
        if (encoded && JSON.stringify(original).slice(1, -1) !== original) {
          throw new Error("Cannot safely restore escaped private value in streamed tool arguments");
        }
        return encoded ? JSON.stringify(original).slice(1, -1) : original;
      });
    }

    function restoreArguments(text: string, sessionID: string): string {
      if (!text) return text;
      try { JSON.parse(text); }
      catch { throw new Error("Cannot safely restore incomplete JSON tool arguments"); }
      if (!text.includes(referencePrefix)) return text;
      // Rewrite only JSON string values, leaving untouched numbers and JSON
      // formatting byte-for-byte intact. A string can itself contain JSON.
      let result = "";
      let cursor = 0;
      while (cursor < text.length) {
        const start = text.indexOf('"', cursor);
        if (start < 0) break;
        let end = start + 1;
        while (end < text.length) {
          if (text.charCodeAt(end) === 92) { end += 2; continue; }
          if (text.charCodeAt(end++) === 34) break;
        }
        const token = text.slice(start, end);
        result += text.slice(cursor, start);
        cursor = end;
        if (text.slice(end).trimStart().startsWith(":")) { result += token; continue; }
        const value = JSON.parse(token) as string;
        if (!value.includes(referencePrefix)) { result += token; continue; }
        let restored: string;
        if (/^\s*[\[{]/.test(value)) {
          try { JSON.parse(value); restored = restoreArguments(value, sessionID); }
          catch (error) {
            if (!(error instanceof SyntaxError)) throw error;
            restored = restoreText(value, sessionID);
          }
        } else restored = restoreText(value, sessionID);
        result += restored === value ? token : JSON.stringify(restored);
      }
      return result + text.slice(cursor);
    }

    // The OpenCode WebSocket hook can return only one frame. Keep partial
    // references per content lane and reject an unfinished lane at its end,
    // rather than silently dropping bytes or returning an invalid extra frame.
    const wsPending = new Map<string, Map<string, string>>();
    const wsEnabled = new Map<string, boolean>();
    const wsArguments = new Map<string, Map<string, string>>();
    function restoreResponsesFrame(frame: string, sessionID: string): string {
      const event = JSON.parse(frame) as Record<string, unknown>;
      const type = String(event.type);
      const family = type.replace(/\.(delta|done)$/, "");
      const lane = JSON.stringify([family, event.item_id, event.content_index, event.summary_index]);
      let pending = wsPending.get(sessionID);
      if (!pending) { pending = new Map(); wsPending.set(sessionID, pending); }
      let argumentsByItem = wsArguments.get(sessionID);
      if (!argumentsByItem) { argumentsByItem = new Map(); wsArguments.set(sessionID, argumentsByItem); }
      const restore = (text: string, encoded = false) => restoreText(text, sessionID, encoded);
      const deltaTypes = ["response.output_text.delta", "response.refusal.delta", "response.reasoning.delta",
        "response.reasoning_text.delta", "response.reasoning_summary_text.delta", "response.function_call_arguments.delta",
        "response.custom_tool_call_input.delta"];
      if (type === "response.function_call_arguments.delta" && typeof event.delta === "string" && typeof event.item_id === "string") {
        argumentsByItem.set(event.item_id, (argumentsByItem.get(event.item_id) ?? "") + event.delta);
        event.delta = "";
      } else if (deltaTypes.includes(type) && typeof event.delta === "string") {
        const text = (pending.get(lane) ?? "") + event.delta;
        const [ready, tail] = splitReferences(text, ref => resolve(sessionID, ref),
          type === "response.function_call_arguments.delta");
        pending.set(lane, tail);
        event.delta = ready;
      } else if (["response.output_text.done", "response.refusal.done", "response.reasoning.done",
        "response.reasoning_text.done", "response.reasoning_summary_text.done", "response.function_call_arguments.done"].includes(type)) {
        if (pending.get(lane)) throw new Error("Cannot safely finish a WebSocket stream with a partial private reference");
        pending.delete(lane);
        if (typeof event.text === "string") event.text = restore(event.text);
        if (typeof event.refusal === "string") event.refusal = restore(event.refusal);
        if (typeof event.arguments === "string") {
          if (typeof event.item_id === "string") argumentsByItem.set(event.item_id, event.arguments);
          event.arguments = restoreArguments(event.arguments, sessionID);
        }
      } else if ((type === "response.output_item.done" || type === "response.output_item.added")
        && event.item && typeof event.item === "object") {
        const item = { ...event.item as Record<string, unknown> };
        if (item.type === "function_call" && typeof item.id === "string") {
          if (type === "response.output_item.added" && typeof item.arguments === "string") {
            argumentsByItem.set(item.id, item.arguments);
            item.arguments = "";
          } else {
            const args = typeof item.arguments === "string" ? item.arguments : argumentsByItem.get(item.id);
            if (args !== undefined) item.arguments = restoreArguments(args, sessionID);
            if (type === "response.output_item.done") argumentsByItem.delete(item.id);
          }
        }
        event.item = walk(item as Json, text => restore(text), "", "content");
      } else if (["response.content_part.done", "response.content_part.added", "response.reasoning_summary_part.done",
        "response.reasoning_summary_part.added"].includes(type) && event.part && typeof event.part === "object") {
        event.part = walk(event.part as Json, text => restore(text), "", "content");
      } else if (["response.completed", "response.incomplete", "response.failed"].includes(type)) {
        if ([...pending.values()].some(Boolean)) throw new Error("Cannot safely finish a WebSocket stream with a partial private reference");
        wsPending.delete(sessionID);
        wsArguments.delete(sessionID);
        if (event.response && typeof event.response === "object") {
          const response = { ...event.response as Record<string, unknown> };
          if (Array.isArray(response.output)) response.output = walk(response.output as Json, text => restore(text), "", "content");
          event.response = response;
        }
      }
      return JSON.stringify(event);
    }

    function restoreResponsesStream(body: ReadableStream<Uint8Array>, sessionID: string): ReadableStream<Uint8Array> {
      const pending = new Map<string, { text: string; template: Record<string, unknown> }>();
      const argumentsByItem = new Map<string, string>();
      const family = (event: Record<string, unknown>) => String(event.type).replace(/\.(delta|done)$/, "");
      const key = (event: Record<string, unknown>) => JSON.stringify([family(event), event.item_id, event.content_index, event.summary_index]);
      const restore = (text: string) => restoreText(text, sessionID);
      const restoreEncoded = (text: string) => restoreText(text, sessionID, true);
      const split = (text: string, encoded: boolean) =>
        splitReferences(text, ref => resolve(sessionID, ref), encoded);
      const render = (event: Record<string, unknown>) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`;
      const flush = (id: string) => {
        const entry = pending.get(id);
        pending.delete(id);
        const encoded = entry?.template.type === "response.function_call_arguments.delta";
        return entry?.text ? render({ ...entry.template, delta: encoded ? restoreEncoded(entry.text) : restore(entry.text) }) : "";
      };
      let buffer = "";
      const process = (packet: string): string => {
        const lines = packet.split(/\r?\n/);
        const data = lines.filter(line => line.startsWith("data:")).map(line => line.slice(5).replace(/^ /, "")).join("\n");
        if (!data || data === "[DONE]") return `${packet}\n\n`;
        let event: Record<string, unknown>;
        try { event = JSON.parse(data); } catch { return `${packet}\n\n`; }
        if (!event || typeof event !== "object") return `${packet}\n\n`;
        if (event.type === "response.function_call_arguments.delta" && typeof event.delta === "string" && typeof event.item_id === "string") {
          argumentsByItem.set(event.item_id, (argumentsByItem.get(event.item_id) ?? "") + event.delta);
          return render({ ...event, delta: "" });
        }
        if (["response.output_text.delta", "response.refusal.delta", "response.reasoning.delta", "response.reasoning_text.delta",
          "response.reasoning_summary_text.delta", "response.function_call_arguments.delta", "response.custom_tool_call_input.delta"].includes(String(event.type))
          && typeof event.delta === "string") {
          const id = key(event);
          const [ready, tail] = split((pending.get(id)?.text ?? "") + event.delta,
            event.type === "response.function_call_arguments.delta");
          pending.set(id, { text: tail, template: event });
          return render({ ...event, delta: ready });
        }
        if ((event.type === "response.output_text.done" || event.type === "response.reasoning.done"
          || event.type === "response.reasoning_text.done" || event.type === "response.reasoning_summary_text.done")
          && typeof event.text === "string") {
          return flush(key(event)) + render({ ...event, text: restore(event.text) });
        }
        if (event.type === "response.refusal.done" && typeof event.refusal === "string") {
          return flush(key(event)) + render({ ...event, refusal: restore(event.refusal) });
        }
        if (event.type === "response.function_call_arguments.done" && typeof event.arguments === "string") {
          if (typeof event.item_id === "string") argumentsByItem.set(event.item_id, event.arguments);
          return render({ ...event, arguments: restoreArguments(event.arguments, sessionID) });
        }
        if ((event.type === "response.output_item.done" || event.type === "response.output_item.added")
          && event.item && typeof event.item === "object") {
          const item = { ...event.item as Record<string, unknown> };
          if (item.type === "function_call" && typeof item.id === "string") {
            if (event.type === "response.output_item.added" && typeof item.arguments === "string") {
              argumentsByItem.set(item.id, item.arguments);
              item.arguments = "";
            } else {
              const args = typeof item.arguments === "string" ? item.arguments : argumentsByItem.get(item.id);
              if (args !== undefined) item.arguments = restoreArguments(args, sessionID);
              if (event.type === "response.output_item.done") argumentsByItem.delete(item.id);
            }
          }
          const restored = walk(item as Json, text => restore(text), "", "content");
          return render({ ...event, item: restored });
        }
        if (["response.content_part.done", "response.content_part.added", "response.reasoning_summary_part.done",
          "response.reasoning_summary_part.added"].includes(String(event.type)) && event.part && typeof event.part === "object") {
          return render({ ...event, part: walk(event.part as Json, text => restore(text), "", "content") });
        }
        if (event.type === "response.completed" || event.type === "response.incomplete" || event.type === "response.failed") {
          const response = event.response && typeof event.response === "object"
            ? { ...event.response as Record<string, unknown> } : event.response;
          if (response && typeof response === "object" && Array.isArray(response.output)) {
            response.output = walk(response.output as Json, text => restore(text), "", "content");
          }
          return [...pending.keys()].map(flush).join("") + render({ ...event, response });
        }
        return `${packet}\n\n`;
      };
      const text = body.pipeThrough(new TextDecoderStream());
      const transformed = text.pipeThrough(new TransformStream<string, string>({
        transform(chunk, controller) {
          buffer += chunk;
          let match: RegExpExecArray | null;
          while ((match = /\r?\n\r?\n/.exec(buffer))) {
            controller.enqueue(process(buffer.slice(0, match.index)));
            buffer = buffer.slice(match.index + match[0].length);
          }
        },
        flush(controller) {
          if (buffer) controller.enqueue(process(buffer));
          for (const id of pending.keys()) controller.enqueue(flush(id));
        },
      }));
      return transformed.pipeThrough(new TextEncoderStream());
    }

    function restoreChatStream(body: ReadableStream<Uint8Array>, sessionID: string): ReadableStream<Uint8Array> {
      type Lane = { tail: string; packet: Record<string, unknown>; choice: number; field: string; tool?: number };
      const lanes = new Map<string, Lane>();
      const tools = new Map<string, { choice: number; index: number; raw: string; packet: Record<string, unknown> }>();
      const emit = (lane: Lane) => {
        if (!lane.tail) return "";
        const delta = lane.tool === undefined ? { [lane.field]: lane.tail }
          : { tool_calls: [{ index: lane.tool, function: { arguments: lane.tail } }] };
        return `data: ${JSON.stringify({ ...lane.packet, choices: [{ index: lane.choice, delta, finish_reason: null }] })}\n\n`;
      };
      const clear = (choice?: number) => {
        let output = "";
        for (const [key, lane] of lanes) {
          if (choice !== undefined && lane.choice !== choice) continue;
          output += emit(lane);
          lanes.delete(key);
        }
        return output;
      };
      const finishTools = (choice?: number) => {
        let output = "";
        for (const [key, tool] of tools) {
          if (choice !== undefined && choice !== tool.choice) continue;
          output += `data: ${JSON.stringify({ ...tool.packet, choices: [{ index: tool.choice, delta: {
            tool_calls: [{ index: tool.index, function: { arguments: restoreArguments(tool.raw, sessionID) } }],
          }, finish_reason: null }] })}\n\n`;
          tools.delete(key);
        }
        return output;
      };
      const packet = (block: string): string => {
        const lines = block.split(/\r?\n/);
        const data = lines.filter(line => line.startsWith("data:")).map(line => line.slice(5).replace(/^ /, "")).join("\n");
        if (!data || data === "[DONE]") return (data === "[DONE]" ? clear() + finishTools() : "") + `${block}\n\n`;
        let event: Record<string, unknown>;
        try { event = JSON.parse(data); } catch { return `${block}\n\n`; }
        if (!event || !Array.isArray(event.choices)) return `${block}\n\n`;
        let before = "";
        let after = "";
        const base = { ...event };
        delete base.choices;
        for (const choice of event.choices) {
          if (!choice || typeof choice !== "object" || typeof choice.index !== "number") continue;
          const index = choice.index as number;
          const delta = choice.delta;
          if (!delta || typeof delta !== "object") {
            if (choice.finish_reason != null) before += clear(index);
            continue;
          }
          const change = (holder: Record<string, unknown>, field: string, tool?: number) => {
            const value = holder[field];
            if (typeof value !== "string") return;
            const id = JSON.stringify([index, field, tool]);
            const [ready, tail] = splitReferences((lanes.get(id)?.tail ?? "") + value,
              ref => resolve(sessionID, ref), tool !== undefined);
            lanes.set(id, { tail, packet: base, choice: index, field, tool });
            holder[field] = ready;
          };
          change(delta, "content");
          change(delta, "reasoning_content");
          if (Array.isArray(delta.tool_calls)) for (const tool of delta.tool_calls) {
            if (tool && typeof tool.index === "number" && tool.function && typeof tool.function === "object") {
              if (typeof tool.function.arguments === "string") {
                const id = JSON.stringify([index, tool.index]);
                const current = tools.get(id) ?? { choice: index, index: tool.index, raw: "", packet: base };
                current.raw += tool.function.arguments;
                tools.set(id, current);
                tool.function.arguments = "";
              }
            }
          }
          if (choice.finish_reason != null) {
            for (const [key, lane] of lanes) {
              if (lane.choice !== index) continue;
              if (lane.tool === undefined && typeof delta[lane.field] === "string") {
                delta[lane.field] += lane.tail;
                lanes.delete(key);
              } else before += clear(index);
            }
            if ([...tools.values()].some(tool => tool.choice === index)) {
              const reason = choice.finish_reason;
              choice.finish_reason = null;
              after += finishTools(index) + `data: ${JSON.stringify({ ...base, choices: [{ index, delta: {}, finish_reason: reason }] })}\n\n`;
            }
          }
        }
        return before + `data: ${JSON.stringify(event)}\n\n` + after;
      };
      let buffer = "";
      return body.pipeThrough(new TextDecoderStream()).pipeThrough(new TransformStream<string, string>({
        transform(chunk, controller) {
          buffer += chunk;
          let match: RegExpExecArray | null;
          while ((match = /\r?\n\r?\n/.exec(buffer))) {
            controller.enqueue(packet(buffer.slice(0, match.index)));
            buffer = buffer.slice(match.index + match[0].length);
          }
        },
        flush(controller) {
          if (buffer) controller.enqueue(packet(buffer));
          controller.enqueue(clear() + finishTools());
        },
      })).pipeThrough(new TextEncoderStream());
    }

    function restoreAnthropicStream(body: ReadableStream<Uint8Array>, sessionID: string): ReadableStream<Uint8Array> {
      const pending = new Map<number, { tail: string; delta: Record<string, unknown> }>();
      const thinking = new Map<number, { text: string; signature: string }>();
      const argumentsByBlock = new Map<number, string>();
      const render = (event: Record<string, unknown>) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`;
      const flush = (index: number) => {
        const entry = pending.get(index);
        pending.delete(index);
        if (!entry?.tail) return "";
        const field = entry.delta.type === "input_json_delta" ? "partial_json"
          : entry.delta.type === "thinking_delta" ? "thinking" : "text";
        return render({ type: "content_block_delta", index, delta: { ...entry.delta, [field]: entry.tail } });
      };
      const packet = (block: string): string => {
        const lines = block.split(/\r?\n/);
        const data = lines.filter(line => line.startsWith("data:")).map(line => line.slice(5).replace(/^ /, "")).join("\n");
        if (!data) return `${block}\n\n`;
        let event: Record<string, unknown>;
        try { event = JSON.parse(data); } catch { return `${block}\n\n`; }
        if (!event || typeof event !== "object") return `${block}\n\n`;
        if (event.type === "content_block_delta" && typeof event.index === "number"
          && event.delta && typeof event.delta === "object") {
          const delta = { ...event.delta as Record<string, unknown> };
          if (delta.type === "input_json_delta" && typeof delta.partial_json === "string") {
            argumentsByBlock.set(event.index, (argumentsByBlock.get(event.index) ?? "") + delta.partial_json);
            return render({ ...event, delta: { ...delta, partial_json: "" } });
          }
          const field = delta.type === "text_delta" ? "text" : delta.type === "input_json_delta" ? "partial_json"
            : delta.type === "thinking_delta" ? "thinking" : undefined;
          if (delta.type === "signature_delta" && typeof delta.signature === "string") {
            const state = thinking.get(event.index) ?? { text: "", signature: "" };
            state.signature += delta.signature;
            thinking.set(event.index, state);
            return flush(event.index) + `${block}\n\n`;
          }
          if (field && typeof delta[field] === "string") {
            if (field === "thinking") {
              const state = thinking.get(event.index) ?? { text: "", signature: "" };
              state.text += delta[field];
              thinking.set(event.index, state);
            }
            const [ready, tail] = splitReferences((pending.get(event.index)?.tail ?? "") + delta[field],
              ref => resolve(sessionID, ref), field === "partial_json");
            pending.set(event.index, { tail, delta });
            delta[field] = ready;
            return render({ ...event, delta });
          }
        }
        if (event.type === "content_block_stop" && typeof event.index === "number") {
          const state = thinking.get(event.index);
          if (state?.signature) {
            signedThinking.set(state.signature, state.text);
          }
          thinking.delete(event.index);
          const raw = argumentsByBlock.get(event.index);
          argumentsByBlock.delete(event.index);
          const argumentsDelta = raw === undefined ? "" : render({ type: "content_block_delta", index: event.index,
            delta: { type: "input_json_delta", partial_json: restoreArguments(raw, sessionID) } });
          return flush(event.index) + argumentsDelta + `${block}\n\n`;
        }
        if (event.type === "message_stop") return [...pending.keys()].map(flush).join("") + `${block}\n\n`;
        if (event.type === "content_block_start" && event.content_block && typeof event.content_block === "object") {
          const initial = event.content_block as Record<string, unknown>;
          if (initial.type === "thinking" && typeof event.index === "number") {
            thinking.set(event.index, { text: typeof initial.thinking === "string" ? initial.thinking : "",
              signature: typeof initial.signature === "string" ? initial.signature : "" });
          }
          const content = walk(event.content_block as Json, text => restoreText(text, sessionID), "", "content");
          return render({ ...event, content_block: content });
        }
        return `${block}\n\n`;
      };
      let buffer = "";
      return body.pipeThrough(new TextDecoderStream()).pipeThrough(new TransformStream<string, string>({
        transform(chunk, controller) {
          buffer += chunk;
          let match: RegExpExecArray | null;
          while ((match = /\r?\n\r?\n/.exec(buffer))) {
            controller.enqueue(packet(buffer.slice(0, match.index)));
            buffer = buffer.slice(match.index + match[0].length);
          }
        },
        flush(controller) {
          if (buffer) controller.enqueue(packet(buffer));
          for (const index of pending.keys()) controller.enqueue(flush(index));
        },
      })).pipeThrough(new TextEncoderStream());
    }

    function restoreGeminiStream(body: ReadableStream<Uint8Array>, sessionID: string): ReadableStream<Uint8Array> {
      const pending = new Map<string, { tail: string; index: number; thought: boolean }>();
      const restore = (text: string) => restoreText(text, sessionID);
      const flush = (id: string) => {
        const entry = pending.get(id);
        pending.delete(id);
        if (!entry?.tail) return "";
        return `data: ${JSON.stringify({ candidates: [{ index: entry.index, content: {
          role: "model", parts: [{ text: entry.tail, ...(entry.thought ? { thought: true } : {}) }],
        } }] })}\n\n`;
      };
      const packet = (block: string): string => {
        const data = block.split(/\r?\n/).filter(line => line.startsWith("data:"))
          .map(line => line.slice(5).replace(/^ /, "")).join("\n");
        if (!data) return `${block}\n\n`;
        let event: Record<string, unknown>;
        try { event = JSON.parse(data); } catch { return `${block}\n\n`; }
        if (!event || !Array.isArray(event.candidates)) return `${block}\n\n`;
        let before = "";
        for (const candidate of event.candidates) {
          if (!candidate || typeof candidate !== "object") continue;
          const index = typeof candidate.index === "number" ? candidate.index : 0;
          const content = candidate.content;
          if (content && typeof content === "object" && Array.isArray(content.parts)) {
            for (let i = 0; i < content.parts.length; i++) {
              const part = content.parts[i];
              if (!part || typeof part !== "object") continue;
              const thought = part.thought === true;
              const id = JSON.stringify([index, thought]);
              if (typeof part.text === "string") {
                const [ready, tail] = splitReferences((pending.get(id)?.tail ?? "") + part.text,
                  ref => resolve(sessionID, ref), false);
                pending.set(id, { tail, index, thought });
                part.text = ready;
              } else {
                for (const [key, entry] of pending) if (entry.index === index) before += flush(key);
              }
              content.parts[i] = walk(part as Json, restore, "", "content");
            }
          }
          if (candidate.finishReason) for (const [key, entry] of pending) if (entry.index === index) {
            const parts = content && typeof content === "object" && Array.isArray(content.parts) ? content.parts : [];
            const last = parts[parts.length - 1];
            if (last && typeof last === "object" && typeof last.text === "string"
              && (last.thought === true) === entry.thought) {
              last.text += entry.tail;
              pending.delete(key);
            } else before += flush(key);
          }
        }
        return before + `data: ${JSON.stringify(event)}\n\n`;
      };
      let buffer = "";
      return body.pipeThrough(new TextDecoderStream()).pipeThrough(new TransformStream<string, string>({
        transform(chunk, controller) {
          buffer += chunk;
          let match: RegExpExecArray | null;
          while ((match = /\r?\n\r?\n/.exec(buffer))) {
            controller.enqueue(packet(buffer.slice(0, match.index)));
            buffer = buffer.slice(match.index + match[0].length);
          }
        },
        flush(controller) {
          if (buffer) controller.enqueue(packet(buffer));
          for (const id of pending.keys()) controller.enqueue(flush(id));
        },
      })).pipeThrough(new TextEncoderStream());
    }

    try {
      registrations.push(await ctx.rpc.register(outputRpc, {
        async resolve(input) {
          if (!await enabled()) return {};
          const { sessionID, references } = input as { sessionID: string; references: string[] };
          return Object.fromEntries(references.flatMap(ref => {
            const original = resolve(sessionID, ref);
            return original === undefined ? [] : [[ref, original]];
          }));
        },
      }));
      registrations.push(await ctx.session.hook<HttpEvent>("http.request", async event => {
        const active = await enabled();
        if (!event.request.body || !active) {
          httpEnabled.set(event.request, active);
          if (event.request.body && !active) {
            verifiedHistory.delete(event.sessionID);
            let state = sessions.get(event.sessionID);
            if (!state) {
              state = { refs: new Map(), scanned: new Set() };
              sessions.set(event.sessionID, state);
            }
            state.continuationBlocked = true;
          }
          return;
        }
        const path = new URL(event.request.url).pathname;
        if (!path.endsWith("/responses") && !path.endsWith("/responses/compact") && !path.endsWith("/chat/completions") && !path.endsWith("/messages")
          && !path.endsWith(":streamGenerateContent") && !path.endsWith(":generateContent")) {
          throw new Error("Unsupported provider protocol for prompt privacy");
        }
        const body = await event.request.clone().text();
        await verifyHistory(event.sessionID);
        const request = JSON.parse(body);
        const masked = redact(body, event.sessionID, typeof request?.previous_response_id === "string");
        if (masked === body) {
          httpEnabled.set(event.request, true);
          return;
        }
        const headers = new Headers(event.request.headers);
        headers.delete("content-length");
        event.request = new Request(event.request, { body: masked, headers });
        httpEnabled.set(event.request, true);
      }));
      registrations.push(await ctx.session.hook<HttpResponseEvent>("http.response", async event => {
        if (!event.response.body || !(httpEnabled.get(event.request) ?? await enabled())) return;
        const path = new URL(event.request.url).pathname;
        const gemini = path.endsWith(":streamGenerateContent") || path.endsWith(":generateContent");
        const responses = path.endsWith("/responses") || path.endsWith("/responses/compact");
        if (!gemini && !responses && !path.endsWith("/chat/completions") && !path.endsWith("/messages")) return;
        const contentType = event.response.headers.get("content-type")?.toLowerCase() ?? "";
        const headers = new Headers(event.response.headers);
        headers.delete("content-length");
        let body: BodyInit;
        if (contentType.includes("text/event-stream")) {
          body = gemini ? restoreGeminiStream(event.response.body, event.sessionID)
            : responses ? restoreResponsesStream(event.response.body, event.sessionID)
            : path.endsWith("/messages") ? restoreAnthropicStream(event.response.body, event.sessionID)
            : restoreChatStream(event.response.body, event.sessionID);
        } else if (contentType.includes("application/json")) {
          const raw = await event.response.text();
          if (!raw.includes(referencePrefix)) {
            if (path.endsWith("/messages")) {
              const value = JSON.parse(raw) as { content?: { type?: string; thinking?: string; signature?: string }[] };
              for (const block of value.content ?? []) {
                if (block.type === "thinking" && typeof block.thinking === "string" && typeof block.signature === "string") {
                  signedThinking.set(block.signature, block.thinking);
                }
              }
            }
            event.response = new Response(raw, { status: event.response.status, statusText: event.response.statusText, headers });
            return;
          }
          const value = JSON.parse(raw) as Record<string, Json>;
          const restore = (text: string) => restoreText(text, event.sessionID);
          if (gemini && Array.isArray(value.candidates)) for (const candidate of value.candidates) {
            if (candidate && !Array.isArray(candidate) && typeof candidate === "object" && candidate.content) {
              candidate.content = walk(candidate.content, restore, "", "content");
            }
          }
          if (responses && Array.isArray(value.output)) {
            for (const item of value.output) {
              if (item && !Array.isArray(item) && typeof item === "object" && item.type === "function_call"
                && typeof item.arguments === "string") item.arguments = restoreArguments(item.arguments, event.sessionID);
            }
            value.output = walk(value.output, restore, "", "content");
          }
          if (path.endsWith("/messages") && Array.isArray(value.content)) {
            for (const block of value.content as Json[]) {
              if (!block || Array.isArray(block) || typeof block !== "object" || block.type !== "thinking"
                || typeof block.thinking !== "string" || typeof block.signature !== "string") continue;
              const thinking = block.thinking;
              const restored = restore(thinking);
              signedThinking.set(block.signature, thinking);
              block.thinking = restored;
            }
            value.content = walk(value.content, restore, "", "content");
          }
          if (path.endsWith("/chat/completions") && Array.isArray(value.choices)) {
            for (const choice of value.choices) {
              if (choice && !Array.isArray(choice) && typeof choice === "object" && choice.message) {
                choice.message = walk(choice.message, restore, "", "content");
              }
            }
          }
          body = JSON.stringify(value);
        } else {
          if (event.response.ok) throw new Error("Unsupported provider response for prompt privacy");
          return;
        }
        event.response = new Response(body, {
          status: event.response.status, statusText: event.response.statusText, headers,
        });
      }));
      registrations.push(await ctx.session.hook<FrameEvent>("experimental.ws.send", async event => {
        const active = await enabled();
        wsEnabled.set(event.sessionID, active);
        if (active) {
          await verifyHistory(event.sessionID);
          wsPending.delete(event.sessionID);
          wsArguments.delete(event.sessionID);
          const request = JSON.parse(event.frame);
          if (request?.type !== "response.create") {
            throw new Error("Unsupported WebSocket protocol for prompt privacy");
          }
          event.frame = redact(event.frame, event.sessionID, typeof request.previous_response_id === "string");
        } else {
          verifiedHistory.delete(event.sessionID);
          let state = sessions.get(event.sessionID);
          if (!state) {
            state = { refs: new Map(), scanned: new Set() };
            sessions.set(event.sessionID, state);
          }
          state.continuationBlocked = true;
        }
      }));
      registrations.push(await ctx.session.hook<FrameEvent>("experimental.ws.receive", async event => {
        if (wsEnabled.get(event.sessionID) ?? await enabled()) event.frame = restoreResponsesFrame(event.frame, event.sessionID);
      }));
      registrations.push(await ctx.tool.hook("execute.before", async event => {
        // Code Mode invokes this hook again for the individual tools. Do not
        // interpolate private values into code or a subagent's model prompt.
        if (event.tool === "shell" || event.tool === "execute" || event.tool === "subagent" || !await enabled()) return;
        let changed = false;
        const input = restoreToolInput(event.input, ref => {
          const original = resolve(event.sessionID, ref);
          if (original === undefined) {
            throw new Error(`Private-value reference is unavailable: ${ref}`);
          }
          changed = true;
          return original;
        });
        if (changed) event.input = input;
      }));
      registrations.push(await ctx.shell.hook("create.before", async event => {
        if (!event.command.includes("FINTWIND_SECRET_") || !await enabled()) return;
        for (const found of event.command.matchAll(reference)) {
          const original = originalByRef.get(found[0]);
          if (original !== undefined) event.env[found[0]] = original;
        }
      }));
    } catch (error) {
      await Promise.all(registrations.map(registration => registration.dispose()));
      throw error;
    }
    return async () => {
      await Promise.all(registrations.map(registration => registration.dispose()));
    };
  },
};
