import { createHash, createHmac, randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import { discover, fieldName, isFieldValue } from "./prompt-privacy-detect.ts";

type Registration = { dispose(): Promise<void> };
type HttpEvent = { sessionID: string; request: Request };
type FrameEvent = { sessionID: string; frame: string };
type ContextEvent = { system: { type: string; text: string }[] };
type ShellEvent = { command: string; env: Record<string, string | undefined> };
type Context = {
  rpc: { register(definition: unknown, handlers: { resolve(input: unknown): Promise<unknown> }): Promise<Registration> };
  tool: { hook(name: "execute.before", callback: (event: ToolEvent) => Promise<void>): Promise<Registration> };
  session: { hook<T>(name: string, callback: (event: T) => void | Promise<void>): Promise<Registration> };
  shell: { hook(name: "create.before", callback: (event: ShellEvent) => Promise<void>): Promise<Registration> };
};

const referenceSource = "FINTWIND_SECRET_[A-F0-9]{24}";
const reference = new RegExp(`\\b${referenceSource}\\b`, "g");
const compactionInstruction = "Preserve any FINTWIND_SECRET_ references needed for unfinished work exactly as written, along with what they are for. Do not resolve them or copy the original private values into the summary.";

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
type FieldScope = "request" | "content" | "text" | "chat-tool-call" | "function" |
  "function-response" | "tool-data" | "encoded-tool-data";

function childScope(parent: { [key: string]: Json }, key: string, scope: FieldScope): FieldScope | undefined {
  switch (scope) {
    case "request":
      if (key === "instructions") return "text";
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
    if (scope === "encoded-tool-data" && /^\s*[\[{]/.test(value)) {
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

type SessionPrivacy = { refs: Map<string, string>; scanned: Set<string>; matcher?: RegExp };
type ProcessPrivacy = { seed: Buffer; originalByRef: Map<string, string>; sessions: Map<string, SessionPrivacy> };
const privacyKey = Symbol.for("fintwind.prompt-privacy.process-state.v1");
const processGlobals = globalThis as typeof globalThis & { [privacyKey]?: ProcessPrivacy };

function processPrivacy(): ProcessPrivacy {
  // A location plugin can unload between turns. Keep the references in this
  // OpenCode process without storing originals on disk or in its environment.
  if (!processGlobals[privacyKey]) {
    const state: ProcessPrivacy = { seed: randomBytes(32), originalByRef: new Map(), sessions: new Map() };
    Object.defineProperty(processGlobals, privacyKey, { value: state, enumerable: false });
  }
  return processGlobals[privacyKey]!;
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

const outputInstruction = "Respond and use tools normally. Use private-value references as written wherever the task needs them; Fintwind restores them for display and tool delivery automatically. For Shell, read the environment variable named by the full reference: $env:NAME in PowerShell or \"$NAME\" in Bash.";

export default {
  id: "fintwind.prompt-privacy",
  async setup(ctx: Context) {
    const settingsPath = process.env.FINTWIND_PROMPT_PRIVACY_SETTINGS;
    const { seed, originalByRef, sessions } = processPrivacy();
    const registrations: Registration[] = [];

    async function enabled(): Promise<boolean> {
      if (!settingsPath) return true;
      try {
        return JSON.parse(await readFile(settingsPath, "utf8")).prompt_privacy_enabled !== false;
      } catch {
        return true;
      }
    }

    function redact(body: string, sessionID: string): string {
      const input: Json = JSON.parse(body);
      let state = sessions.get(sessionID);
      if (!state) {
        state = { refs: new Map(), scanned: new Set() };
        sessions.set(sessionID, state);
      }
      const { refs, scanned } = state;
      const oldSize = refs.size;
      function remember(spelling: string, original = spelling) {
        let ref = refs.get(original);
        if (!ref) {
          const suffix = createHmac("sha256", seed).update(sessionID).update("\0").update(original)
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
      });
      if (oldSize !== refs.size) {
        const wordCharacter = "[\\p{L}\\p{M}\\p{N}_]";
        const patterns = [...refs.keys()].sort((first, second) => second.length - first.length).map(value => {
          const pattern = escapeRegex(value);
          return value.length < 8 ? `(?<!${wordCharacter})${pattern}(?!${wordCharacter})` : pattern;
        });
        state.matcher = new RegExp(`${referenceSource}|${patterns.join("|")}`, "gu");
      }
      if (!state.matcher) return body;
      const next = walk(input, text => text.replace(state.matcher!, value => refs.get(value) ?? value));
      return next === input ? body : JSON.stringify(next);
    }

    function resolve(sessionID: string, ref: string): string | undefined {
      const original = originalByRef.get(ref);
      return original !== undefined && sessions.get(sessionID)?.refs.get(original) === ref ? original : undefined;
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
      registrations.push(await ctx.session.hook<ContextEvent>("context", async event => {
        if (await enabled()) event.system.push({ type: "text", text: outputInstruction });
      }));
      registrations.push(await ctx.session.hook<ContextEvent>("compaction", async event => {
        if (await enabled()) event.system.push({ type: "text", text: compactionInstruction });
      }));
      registrations.push(await ctx.session.hook<HttpEvent>("http.request", async event => {
        if (!event.request.body || !await enabled()) return;
        const body = await event.request.clone().text();
        const masked = redact(body, event.sessionID);
        if (masked === body) return;
        const headers = new Headers(event.request.headers);
        headers.delete("content-length");
        event.request = new Request(event.request, { body: masked, headers });
      }));
      registrations.push(await ctx.session.hook<FrameEvent>("experimental.ws.send", async event => {
        if (await enabled()) event.frame = redact(event.frame, event.sessionID);
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
