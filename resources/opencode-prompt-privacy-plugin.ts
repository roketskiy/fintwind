import { createHash, createHmac, randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";

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
const tokens = /(?<![A-Za-z0-9_-])(?:gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{82}|glpat-[A-Za-z0-9_-]{20}|sk-ant-(?:api\d{2}|admin\d{2})-[A-Za-z0-9_-]{40,255}|sk-(?:proj|svcacct|admin)-[A-Za-z0-9_-]{40,255}|sk-[A-Za-z0-9]{20}T3BlbkFJ[A-Za-z0-9]{20}|xox[baprs]-\d{10,13}-\d{10,13}(?:-[A-Za-z0-9]{8,64}){1,3}|hf_[A-Za-z]{34}|(?:sk|rk)_(?:test|live|prod)_[A-Za-z0-9]{10,99}|AIza[A-Za-z0-9_-]{35}|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,})(?![A-Za-z0-9_-])/g;
const email = /(?<![A-Za-z0-9.!#$%&'*+/=?^_`{}~@-])[A-Za-z0-9!#$%&'*+/=?^_`{}~-]+(?:\.[A-Za-z0-9!#$%&'*+/=?^_`{}~-]+)*@(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)+[A-Za-z]{2,63}(?![A-Za-z0-9@-]|\.[A-Za-z0-9])/g;
const keyMarker = /-----(BEGIN|END) ((?:[A-Z0-9_-]+ ){0,4}PRIVATE KEY(?: BLOCK)?)-----/g;
const keyBody = /^(?:[A-Za-z0-9+/= \t\r\n]|\\[rn])+$/;
const field = /(?<![A-Za-z0-9])(?<name>password|passwd|passphrase|pgpassword|(?:db|database|user|admin|redis|mysql|postgres|smtp)password|api[_-]?key|(?:access|refresh|auth|api|id|session|bot)[_-]?token|token|密码|访问令牌|API密钥)['"]?[ \t]*[:=：][ \t]*|--(?<flag>password|passwd|passphrase|api[-_]?key|token)[ \t]+|\bauthorization['"]?[ \t]*:[ \t]*["']?bearer[ \t]+/gi;
const fieldName = /(?:^|[_-])(?:password|passwd|passphrase|pgpassword|(?:db|database|user|admin|redis|mysql|postgres|smtp)password|api[_-]?key|(?:access|refresh|auth|api|id|session|bot)[_-]?token|token|密码|访问令牌|API密钥)$/i;
const passwordName = /(?:password|passwd|passphrase|密码)$/i;
const fieldValue = /^(?:"((?:[^"\\\r\n]|\\[^\r\n])*)"|'((?:[^'\\\r\n]|\\[^\r\n])*)'|`([^`\r\n]*)`|([^\s,;#&<>{}\[\]()'"`，。；、：]+))/;
const placeholder = /^(?:your[-_ ].+|example(?:[-_ ].+)?|secret_\d+|fintwind_secret_[a-f0-9]{24}|\$.*|%[a-z_][a-z0-9_]*%|(?:process\.env|os\.environ|os\.getenv)[.\[(].*|[*.•]+|<[^<>]*>|\[[A-Z_ ]+\]|changeme|placeholder|redacted|null|none|true|false|disabled|enabled|token|password|secret|undefined|dummy)$/i;

function isFieldValue(text: string, name: string): boolean {
  const value = text.trim();
  return value.length >= (passwordName.test(name) ? 1 : 8) && !placeholder.test(value);
}

function* discover(text: string): Iterable<{ spelling: string; original?: string }> {
  for (const match of text.matchAll(tokens)) yield { spelling: match[0] };
  if (text.includes("@")) {
    for (const match of text.matchAll(email)) yield { spelling: match[0] };
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
const wireFields = new Set([
  "model", "role", "type", "id", "name", "tool_call_id", "call_id", "item_id", "response_id", "previous_response_id",
  "media_type", "mime_type", "signature", "thoughtSignature", "encrypted_content",
]);

type FieldScope = "protocol" | "tool-data" | "encoded-tool-data" | "media";

function childScope(parent: { [key: string]: Json }, parentKey: string, key: string): FieldScope {
  // Preserve binary media, not arbitrary text that happens to start with `data:`.
  if ((key === "url" && parentKey === "image_url" && typeof parent[key] === "string" && parent[key].startsWith("data:"))
    || (key === "image_url" && parent.type === "input_image" && typeof parent[key] === "string" && parent[key].startsWith("data:"))
    || (key === "file_data" && (parent.type === "input_file" || parentKey === "file"))
    || (key === "data" && (parentKey === "input_audio" || parentKey === "inlineData" || parentKey === "inline_data"
      || (parentKey === "source" && parent.type === "base64")))) {
    return "media";
  }
  // OpenAI Chat Completions and Responses carry function arguments as JSON text.
  if (key === "arguments" && (parentKey === "function" || parent.type === "function_call")) {
    return "encoded-tool-data";
  }
  // Anthropic and Gemini expose tool payloads as structured JSON instead.
  if ((key === "input" && (parent.type === "tool_use" || parent.type === "server_tool_use"
      || parent.type === "custom_tool_call"))
    || (key === "args" && parentKey === "functionCall")
    || (key === "response" && parentKey === "functionResponse")) {
    return "tool-data";
  }
  return "protocol";
}

function walk(
  value: Json,
  visit: (text: string, key: string) => string,
  key = "",
  scope: FieldScope = "protocol",
): Json {
  if (typeof value === "string") {
    if (scope === "media" || (scope === "protocol" && wireFields.has(key))) return value;
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
  const next = entries.map(([name, child]) => [name, walk(child, visit, name,
    scope === "protocol" ? childScope(value, key, name) : "tool-data")] as const);
  return next.every((entry, index) => entry[1] === entries[index][1]) ? value : Object.fromEntries(next);
}

function escapeRegex(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export default {
  id: "fintwind.prompt-privacy",
  async setup(ctx: Context) {
    const settingsPath = process.env.FINTWIND_PROMPT_PRIVACY_SETTINGS;
    const seed = randomBytes(32);
    const originalByRef = new Map<string, string>();
    const sessions = new Map<string, { refs: Map<string, string>; scanned: Set<string>; matcher?: RegExp }>();
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
        const keys = [...refs.keys()].sort((a, b) => b.length - a.length);
        // Discovery decides what is private. Once known, even a short value
        // can be embedded in other text. Keep existing references intact.
        state.matcher = new RegExp(`${referenceSource}|${keys.map(escapeRegex).join("|")}`, "gu");
      }
      if (!state.matcher) return body;
      const next = walk(input, text => text.replace(state.matcher!, value => refs.get(value) ?? value));
      return next === input ? body : JSON.stringify(next);
    }

    try {
      registrations.push(await ctx.session.hook<ContextEvent>("context", async event => {
        if (await enabled()) event.system.push({ type: "text", text: instruction });
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
      sessions.clear();
      originalByRef.clear();
    };
  },
};
