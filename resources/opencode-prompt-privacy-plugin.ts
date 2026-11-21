import { createHash, createHmac, randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import rules from "./prompt-privacy-rules.json";

type Registration = { dispose(): Promise<void> };
type HttpEvent = { sessionID: string; request: Request };
type FrameEvent = { sessionID: string; frame: string };
type ContextEvent = { system: { type: string; text: string }[] };
type ShellEvent = { command: string; env: Record<string, string | undefined> };
type Context = {
  session: { hook<T>(name: string, callback: (event: T) => void | Promise<void>): Promise<Registration> };
  shell: { hook(name: "create.before", callback: (event: ShellEvent) => Promise<void>): Promise<Registration> };
};

const referenceSource = "FINTWIND_SECRET_[A-F0-9]{24}";
const reference = new RegExp(`\\b${referenceSource}\\b`, "g");
const instruction = "FINTWIND_SECRET_ followed by 24 hexadecimal characters is a local private-value reference. To use it in a shell command, read the environment variable whose name is the complete reference. A program that reads its own environment takes the bare name as its key. For shell expansion use $env:NAME in PowerShell or \"$NAME\" in Bash. Never guess the original value.";
const reuseInstruction = "Reuse the exact reference from an earlier turn directly in each shell command that needs it. Do not run separate lookup or test commands, and do not print the value. If a reference is unavailable, report that instead of using an empty value or searching for the original.";
const compactionInstruction = "Preserve any FINTWIND_SECRET_ references needed for unfinished work exactly as written, along with what they are for. Do not resolve them or copy the original private values into the summary.";
const tokens = new RegExp(rules.tokens, "g");
// Selected provider formats and context-plus-entropy are adapted from
// Gitleaks v8.30.1 and CredSweeper v1.19.6 (see docs/licenses).
const providerTokens = new RegExp(rules.providerTokens, "g");
const webhook = new RegExp(rules.webhook, "g");
const genericKey = new RegExp(rules.genericKey, "gi");
const genericKeyExcludedName = new RegExp(rules.genericKeyExcludedName, "i");
const genericKeyPlainValue = new RegExp(rules.genericKeyPlainValue);
const email = new RegExp(rules.email, "g");
const keyMarker = new RegExp(rules.keyMarker, "g");
const keyBody = new RegExp(rules.keyBody);
const field = new RegExp(rules.field, "gi");
const fieldName = new RegExp(rules.fieldName, "i");
const passwordName = new RegExp(rules.passwordName, "i");
const uriCredential = new RegExp(rules.uriCredential, "gi");
const authToken = new RegExp(rules.authToken, "gi");
const weakName = new RegExp(rules.weakName, "i");
const weakValue = new RegExp(rules.weakValue);
const fieldValue = new RegExp(rules.fieldValue);
const placeholder = new RegExp(rules.placeholder, "i");

function isFieldValue(text: string, name: string): boolean {
  const value = text.trim();
  return value.length >= (passwordName.test(name) ? 1 : 8) && !placeholder.test(value)
    && (!weakName.test(name) || weakValue.test(value));
}

function keyEntropy(value: string): number {
  const counts = new Uint16Array(128);
  for (let i = 0; i < value.length; i++) counts[value.charCodeAt(i)]++;
  let entropy = 0;
  for (const count of counts) {
    if (!count) continue;
    const p = count / value.length;
    entropy -= p * Math.log2(p);
  }
  return entropy;
}

function* discover(text: string): Iterable<{ spelling: string; original?: string }> {
  for (const match of text.matchAll(tokens)) yield { spelling: match[0] };
  for (const match of text.matchAll(providerTokens)) yield { spelling: match[0] };
  for (const match of text.matchAll(webhook)) yield { spelling: match[1] };
  for (const match of text.matchAll(genericKey)) {
    const [, name, spelling] = match;
    if (!genericKeyExcludedName.test(name) && !placeholder.test(spelling)
      && !genericKeyPlainValue.test(spelling) && keyEntropy(spelling) >= 3.1) yield { spelling };
  }
  for (const match of text.matchAll(authToken)) {
    const spelling = match[1] ?? match[2];
    if (isFieldValue(spelling, "")) yield { spelling };
  }
  if (text.includes("@")) {
    const credentials: [number, number][] = [];
    for (const match of text.matchAll(uriCredential)) {
      const end = match.index! + match[0].length - 1;
      credentials.push([end - match[1].length, end]);
      if (isFieldValue(match[2], "password")) yield { spelling: match[1] };
    }
    let index = 0;
    for (const match of text.matchAll(email)) {
      const start = match.index!;
      while (credentials[index]?.[1] <= start) index++;
      const credential = credentials[index];
      if (!credential || credential[0] > start) yield { spelling: match[0] };
    }
  }
  if (text.includes("PRIVATE KEY")) {
    let opened: RegExpMatchArray | undefined;
    for (const match of text.matchAll(keyMarker)) {
      if (match[1] === "BEGIN") opened = match;
      else if (opened) {
        const body = text.slice(opened.index! + opened[0].length, match.index!);
        if (opened[2] === match[2] && body.length >= 32 && (keyBody.test(body) || match[2] === "PGP PRIVATE KEY BLOCK")) {
          yield { spelling: text.slice(opened.index!, match.index! + match[0].length) };
        }
        opened = undefined;
      }
    }
  }
  for (const match of text.matchAll(field)) {
    const matchValue = fieldValue.exec(text.slice(match.index! + match[0].length));
    const spelling = matchValue?.slice(1).find(candidate => candidate !== undefined);
    if (spelling === undefined) continue;
    const name = match.groups?.name ?? match.groups?.flag ?? "";
    // JSON can appear inside prompt text. Decode only a double-quoted JSON
    // member, not shell arguments or environment-file values.
    if (name && matchValue?.[1]?.includes("\\") && text[match.index! - 1] === '"'
      && match[0].slice(name.length).trim() === '":') {
      let original: string | undefined;
      try { original = JSON.parse(matchValue[0]); } catch { /* Keep the literal spelling. */ }
      if (original !== undefined) {
        if (isFieldValue(original, name)) yield { spelling, original };
        continue;
      }
    }
    if (isFieldValue(spelling, name)) yield { spelling };
  }
}

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

    try {
      registrations.push(await ctx.session.hook<ContextEvent>("context", async event => {
        if (await enabled()) event.system.push({ type: "text", text: `${instruction} ${reuseInstruction}` });
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
