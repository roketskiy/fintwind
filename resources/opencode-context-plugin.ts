type Registration = { dispose(): Promise<void> };
type ContextEvent = {
  sessionID: string;
  model?: { providerID?: unknown; id?: unknown };
  system?: unknown;
  messages?: unknown;
  tools?: unknown;
};
type HttpRequestEvent = { sessionID?: unknown; kind?: unknown };
type HookKind = "context" | "http.request" | "http.response";
type Composition = {
  version: 2;
  model: string;
  system: number;
  tools: number;
  memory: number;
  messages: number;
  reasoning: number;
  toolCalls: number;
  files: number;
  toolResults: number;
  opaqueReasoning: boolean;
  opaqueMemory: boolean;
  incomplete: boolean;
};
type ToolInput = Record<string, unknown>;
type ToolExecution = { sessionID: string; signal: AbortSignal };
type PluginTool = {
  name: string;
  description: string;
  input: ToolInput;
  options: { codemode: false };
  execute(input: ToolInput, context: ToolExecution): Promise<{ content: string }>;
};
type ToolEditor = {
  add(tool: PluginTool): void;
  list(): readonly { id: string }[];
  get(id: string): { id: string } | undefined;
  remove(id: string): void;
};
type Context = {
  storage: {
    get(key: string): Promise<unknown>;
    set(key: string, value: unknown): Promise<void>;
    remove(key: string): Promise<void>;
  };
  tool: {
    transform(edit: (editor: ToolEditor) => void): Promise<Registration>;
    list(): Promise<readonly { id: string }[]>;
  };
  session: {
    get(input: { sessionID: string }): Promise<unknown>;
    context(input: { sessionID: string }): Promise<unknown>;
    hook<T>(kind: HookKind, handler: (event: T) => void | Promise<void>): Promise<Registration>;
  };
  rpc: { register(definition: unknown, handlers: { get(input: unknown): Promise<Composition | null> }) : Promise<Registration> } & {
    register(definition: unknown, handlers: Record<string, (input: unknown) => Promise<unknown>>): Promise<Registration>;
  };
};

// The mermaid check capability arrives through the environment, the same way
// the browser plugin receives its bridge. OpenCode re-imports plugins for
// each location: keep one process-local copy before removing the variables,
// so later locations work without forwarding credentials to spawned shell
// processes. Other trusted plugins in the same process are not an OS
// security boundary.
const capabilityKey = Symbol.for("fintwind.mermaid-check.capability.v1");
type CheckCapability = Readonly<{ address: string; token: string }>;
const processGlobals = globalThis as typeof globalThis & { [key: symbol]: CheckCapability | undefined };
let checkCapability = processGlobals[capabilityKey];
if (!checkCapability && process.env.FINTWIND_MERMAID_CHECK_ADDRESS && process.env.FINTWIND_MERMAID_CHECK_TOKEN) {
  checkCapability = Object.freeze({
    address: process.env.FINTWIND_MERMAID_CHECK_ADDRESS,
    token: process.env.FINTWIND_MERMAID_CHECK_TOKEN,
  });
  Object.defineProperty(processGlobals, capabilityKey, { value: checkCapability, enumerable: false });
}
const checkAddress = checkCapability?.address;
const checkToken = checkCapability?.token;
delete process.env.FINTWIND_MERMAID_CHECK_ADDRESS;
delete process.env.FINTWIND_MERMAID_CHECK_TOKEN;

function object(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  return Object.fromEntries(Object.entries(value));
}

function estimateText(value: string): number {
  let ascii = 0;
  let other = 0;
  for (const character of value) {
    if (character.charCodeAt(0) < 128) ascii++;
    else other++;
  }
  return Math.ceil(ascii / 4) + other;
}

function estimateValue(value: unknown): number {
  try {
    const serialized = JSON.stringify(value);
    return serialized === undefined ? 0 : estimateText(serialized);
  }
  catch { return 0; }
}

function parseComposition(value: unknown): Composition | undefined {
  const saved = object(value);
  if (!saved || saved.version !== 2 || typeof saved.model !== "string"
    || typeof saved.incomplete !== "boolean" || typeof saved.opaqueReasoning !== "boolean"
    || typeof saved.opaqueMemory !== "boolean") return undefined;
  const keys = ["system", "tools", "memory", "messages", "reasoning", "toolCalls", "files", "toolResults"];
  if (keys.some(key => typeof saved[key] !== "number" || !Number.isSafeInteger(saved[key]) || saved[key] < 0)) return undefined;
  return {
    version: 2, model: saved.model,
    system: Number(saved.system), tools: Number(saved.tools), memory: Number(saved.memory),
    messages: Number(saved.messages), reasoning: Number(saved.reasoning), toolCalls: Number(saved.toolCalls),
    files: Number(saved.files), toolResults: Number(saved.toolResults),
    opaqueReasoning: saved.opaqueReasoning, opaqueMemory: saved.opaqueMemory, incomplete: saved.incomplete,
  };
}

function resultContent(value: unknown, counts: Composition): number {
  const result = object(value);
  if (!result || typeof result.type !== "string") {
    // Error results have a structured error/content envelope instead of a value.
    if (result && result.error !== undefined) return estimateValue({ error: result.error, content: result.content });
    counts.incomplete = true;
    return 0;
  }
  if (result.type !== "content") {
    return typeof result.value === "string" ? estimateText(result.value) : estimateValue(result.value);
  }
  if (!Array.isArray(result.value)) { counts.incomplete = true; return 0; }
  let estimate = 0;
  for (const item of result.value) {
    const content = object(item);
    if (content?.type === "text" && typeof content.text === "string") estimate += estimateText(content.text);
    else counts.incomplete = true;
  }
  return estimate;
}

function opaquePart(part: Record<string, unknown>): boolean {
  if (part.encrypted !== undefined) return true;
  const metadata = object(part.providerMetadata);
  if (!metadata) return false;
  if (typeof part.text !== "string" || !part.text) return Object.keys(metadata).length > 0;
  return Object.values(metadata).some(raw => {
    const state = object(raw);
    return state !== undefined && ["reasoningEncryptedContent", "encrypted_content", "encryptedContent", "encrypted", "signature"]
      .some(key => state[key] !== undefined);
  });
}

type ContextSources = { summaries: Set<string>; instructions: Set<string> };

function contextSources(value: unknown): ContextSources {
  if (!Array.isArray(value)) throw new Error("Missing session context provenance");
  const sources: ContextSources = { summaries: new Set(), instructions: new Set() };
  for (const raw of value) {
    const message = object(raw);
    if (typeof message?.id !== "string") continue;
    if (message.type === "compaction" && message.status === "completed") sources.summaries.add(message.id);
    if (message.type === "system" || message.type === "skill" || message.type === "synthetic") sources.instructions.add(message.id);
  }
  return sources;
}

function composition(event: ContextEvent, sources: ContextSources): Composition | undefined {
  if (!event.sessionID || !Array.isArray(event.system) || !Array.isArray(event.messages)) return undefined;
  const provider = event.model?.providerID;
  const modelId = event.model?.id;
  if (typeof provider !== "string" || typeof modelId !== "string") return undefined;
  const counts: Composition = {
    version: 2, model: `${provider}/${modelId}`, system: 0, tools: 0, memory: 0, messages: 0,
    reasoning: 0, toolCalls: 0, files: 0, toolResults: 0,
    opaqueReasoning: false, opaqueMemory: false, incomplete: false,
  };
  for (const raw of event.system) {
    const part = object(raw);
    if (part?.type === "text" && typeof part.text === "string") counts.system += estimateText(part.text);
    else counts.incomplete = true;
  }
  const rawTools = object(event.tools);
  if (rawTools) {
    for (const [name, raw] of Object.entries(rawTools)) {
      const tool = object(raw);
      if (tool) counts.tools += estimateValue({ name, description: tool.description, input: tool.input });
      else counts.incomplete = true;
    }
  } else counts.incomplete = true;
  const calls = new Map<string, string>();
  for (const rawMessage of event.messages) {
    const message = object(rawMessage);
    if (!message || !Array.isArray(message.content)) { counts.incomplete = true; continue; }
    for (const rawPart of message.content) {
      const part = object(rawPart);
      if (!part || typeof part.type !== "string") { counts.incomplete = true; continue; }
      switch (part.type) {
        case "text":
          if (typeof part.text === "string") {
            const summary = typeof message.id === "string" && sources.summaries.has(message.id);
            const instruction = message.role === "system" || (typeof message.id === "string" && sources.instructions.has(message.id));
            const attachment = object(object(part.metadata)?.attachment);
            const target = summary ? "memory" : instruction ? "system" : attachment ? "files" : "messages";
            counts[target] += estimateText(part.text);
          } else counts.incomplete = true;
          break;
        case "reasoning":
          if (typeof part.text === "string") counts.reasoning += estimateText(part.text);
          if (opaquePart(part)) { counts.opaqueReasoning = true; counts.incomplete = true; }
          break;
        case "tool-call":
          counts.toolCalls += estimateValue({ name: part.name, input: part.input });
          if (typeof part.id === "string" && typeof part.name === "string") calls.set(part.id, part.name);
          break;
        case "tool-result": {
          const name = typeof part.name === "string" ? part.name : typeof part.id === "string" ? calls.get(part.id) : undefined;
          const target = name === "read" || name === "read_file" ? "files" : "toolResults";
          counts[target] += resultContent(part.result, counts);
          break;
        }
        case "compaction":
          if (typeof part.text === "string") counts.memory += estimateText(part.text);
          if (opaquePart(part)) { counts.opaqueMemory = true; counts.incomplete = true; }
          break;
        case "media":
          counts.incomplete = true;
          break;
        case "effort":
          break;
        default:
          counts.incomplete = true;
      }
    }
  }
  return counts;
}

const input = { type: "object", properties: { sessionID: { type: "string" } }, required: ["sessionID"] };
const definition = {
  id: "fintwind.context",
  methods: {
    get: { input, output: {} },
    ttft: { input, output: {} },
  },
  events: {},
};

// Immutable versions: changing these bytes would invalidate enabled sessions'
// prompt prefixes. Future wording needs a new version, retaining the old ones.
const MERMAID_PROMPT_V1 = [
  "Fintwind Mermaid visualization (v1).",
  "Proactively include a concise fenced mermaid code block when explaining nontrivial architecture, relationships, data flow, or a multi-step process that benefits from visualization, even when a diagram was not explicitly requested. Do not explain such structure only in prose.",
  "Prefer simple flowchart or sequenceDiagram syntax, short quoted labels, and the user's language. Fintwind renders Mermaid blocks directly in the conversation.",
  "Use plain Mermaid without HTML labels, external images, clickable links, icons, custom CSS, or theme directives. Output the diagram source directly; no rendering tools or generated files are needed unless requested.",
  "Do not force diagrams into simple answers. Follow explicit user requests for a different format or workflow.",
].join("\n");

const MERMAID_PROMPT_V2 = [
  "Fintwind Mermaid visualization (v2).",
  "Proactively include a concise fenced mermaid code block when explaining nontrivial architecture, relationships, data flow, or a multi-step process that benefits from visualization, even when a diagram was not explicitly requested. Do not explain such structure only in prose.",
  "Prefer flowchart or sequenceDiagram syntax with short quoted labels in the user's language. Fintwind renders Mermaid blocks directly in the conversation.",
  "Use plain Mermaid without HTML labels, external images, clickable links, icons, custom CSS, or theme directives. Output the diagram source directly.",
  "After writing or changing a mermaid code block, validate its exact source with the fintwind_mermaid_check tool. Fix every reported error and revalidate until the diagram is valid; a block that cannot be made valid is removed rather than left broken.",
  "Do not force diagrams into simple answers. Follow explicit user requests for a different format or workflow.",
].join("\n");

// v3 removes the style preset (flowchart-or-sequence, short labels) so the
// model can pick whatever diagram type and wording the idea needs. The renderer
// limits stay because the native renderer genuinely cannot draw them.
const MERMAID_PROMPT_V3 = [
  "Fintwind Mermaid visualization (v3).",
  "Proactively include a fenced mermaid code block when explaining nontrivial architecture, relationships, data flow, states, hierarchies, schedules, or any multi-step process that benefits from visualization, even when a diagram was not explicitly requested. Do not explain such structure only in prose.",
  "Choose whichever diagram type expresses the idea best — flowchart, sequenceDiagram, stateDiagram-v2, classDiagram, erDiagram, gantt, pie, timeline, mindmap, quadrantChart, or another supported type — and write the labels in the user's language. Quote labels that contain punctuation or reserved words.",
  "Fintwind renders Mermaid natively and offline: use plain Mermaid text with standard syntax. Avoid HTML labels, images, icons, clickable links, custom CSS, and theme or init directives, which the renderer cannot draw. Output the diagram source directly.",
  "After writing or changing a mermaid code block, validate its exact source with the fintwind_mermaid_check tool. Fix every reported error and revalidate until the diagram is valid; a block that cannot be made valid is removed rather than left broken.",
  "Do not force diagrams into simple answers. Follow explicit user requests for a different format or workflow.",
].join("\n");

// v4 documents Ondine's host-owned theme and permits native node color styles.
const MERMAID_PROMPT_V4 = [
  "Fintwind Mermaid visualization (v4).",
  "Proactively include a fenced mermaid code block when explaining nontrivial architecture, relationships, data flow, states, hierarchies, schedules, or any multi-step process that benefits from visualization, even when a diagram was not explicitly requested. Do not explain such structure only in prose.",
  "Choose whichever supported diagram type expresses the idea best and write labels in the user's language. Quote labels that contain punctuation or reserved words.",
  "Fintwind renders Mermaid natively and offline with Ondine. Output Mermaid source directly. Avoid HTML labels, images, icons, clickable links, external stylesheets, arbitrary CSS, and theme or init directives.",
  "Follow Ondine's official theming guide (https://ondine.sh/docs/guides/theming): the host supplies --ondine-bg and --ondine-fg for background and text, --ondine-surface and --ondine-surface-alt for fills, --ondine-border and --ondine-line for outlines and connectors, --ondine-accent and --ondine-critical for emphasis, and --ondine-series-1 through --ondine-series-8 for chart colors. Fintwind resolves these variables to native colors for its current light or dark theme. The guide's page-level CSS, light-dark(), font, radius, and stroke settings belong to the host, not to Mermaid code fences; do not output a stylesheet or pretend Mermaid config changes them.",
  "Use a small, consistent set of colors when they clarify modules, groups, or states; do not default every complex architecture or flowchart to monochrome, or color every node differently. Native Mermaid node style statements for fill, stroke, and color are allowed. Prefer host theme variables, for example: style A fill:var(--ondine-surface),stroke:var(--ondine-series-2),color:var(--ondine-fg). Keep the same color for the same role, preserve readable text in both themes, and also identify groups or states through labels or shapes. Charts receive a categorical palette automatically.",
  "After writing or changing a mermaid code block, validate its exact source with the fintwind_mermaid_check tool. Fix every reported error and revalidate until the diagram is valid; a block that cannot be made valid is removed rather than left broken.",
  "Do not force diagrams into simple answers. Follow explicit user requests for a different format or workflow.",
].join("\n");

const MERMAID_CHECK_TOOL = "fintwind_mermaid_check";
const MAX_CHECK_SOURCE = 32 * 1024;

type CheckReply = { requestId: string; ok: boolean; error?: string; width?: number; height?: number };

/**
 * One short-lived authenticated connection per check. The reply is a single
 * bounded line, so every response above the reply bound is a protocol error
 * rather than data.
 */
function checkDiagram(source: string, signal: AbortSignal): Promise<CheckReply> {
  if (!checkAddress || !checkToken
    || !/^ws:\/\/127\.0\.0\.1:\d+\/v1\/mermaid-check$/.test(checkAddress)) {
    return Promise.reject(new Error("Fintwind mermaid check is unavailable in this process."));
  }
  if (new TextEncoder().encode(source).length > MAX_CHECK_SOURCE) {
    return Promise.reject(new Error(
      `The diagram source exceeds the ${MAX_CHECK_SOURCE / 1024} KiB limit. No check was issued.`));
  }
  const requestId = crypto.randomUUID();
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(checkAddress);
    let authenticated = false;
    let finished = false;
    const end = (error?: Error, value?: CheckReply): void => {
      if (finished) return;
      finished = true;
      clearTimeout(timeout);
      signal.removeEventListener("abort", cancel);
      if (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING) socket.close();
      if (error) reject(error); else resolve(value!);
    };
    const cancel = (): void => end(new Error("Mermaid check cancelled."));
    const timeout = setTimeout(() => end(new Error("Mermaid check timed out.")), 10_000);
    signal.addEventListener("abort", cancel, { once: true });
    socket.onopen = (): void => {
      if (finished || signal.aborted) { cancel(); return; }
      socket.send(JSON.stringify({ type: "hello", version: 1, token: checkToken }));
    };
    socket.onmessage = (event): void => {
      if (finished) return;
      try {
        if (typeof event.data !== "string" || event.data.length > 8 * 1024) {
          throw new Error("Invalid mermaid check response.");
        }
        const reply = JSON.parse(event.data) as { type?: string; version?: number } & Partial<CheckReply>;
        if (!authenticated && reply.type === "hello" && reply.version === 1) {
          authenticated = true;
          socket.send(JSON.stringify({ type: "check", requestId, source }));
          return;
        }
        if (!authenticated || reply.type !== "result" || reply.requestId !== requestId) {
          throw new Error("Fintwind mermaid check rejected the call.");
        }
        end(undefined, reply as CheckReply);
      } catch (error) {
        end(error instanceof Error ? error : new Error("Invalid mermaid check response."));
      }
    };
    socket.onerror = (): void => end(new Error("Fintwind mermaid check connection failed."));
    socket.onclose = (): void => end(new Error("Fintwind mermaid check disconnected."));
    if (signal.aborted) cancel();
  });
}

const mermaidCheckTool: PluginTool = {
  name: MERMAID_CHECK_TOOL,
  description: "Validate one Mermaid diagram source against Fintwind's native renderer. " +
    "Returns the size the diagram draws at, or the exact compiler error with its line and column. " +
    "Call it after writing or changing a mermaid code fence, and after every fix, so the conversation never shows a broken diagram.",
  options: { codemode: false },
  input: {
    type: "object",
    properties: { source: { type: "string", minLength: 1, maxLength: MAX_CHECK_SOURCE } },
    required: ["source"],
    additionalProperties: false,
  },
  async execute(raw, context) {
    const source = Object(raw).source;
    if (typeof source !== "string" || !source.trim()) {
      throw new Error("Give the exact diagram source to validate. Nothing was checked.");
    }
    const reply = await checkDiagram(source, context.signal);
    if (reply.ok) {
      const width = typeof reply.width === "number" ? Math.round(reply.width) : 0;
      const height = typeof reply.height === "number" ? Math.round(reply.height) : 0;
      return { content: `Valid diagram. Fintwind draws it at ${width}x${height} px.` };
    }
    return { content: `Invalid diagram: ${reply.error ?? "unknown error"}` };
  },
};

export default {
  id: "fintwind.context",
  async setup(ctx: Context) {
    const latest = new Map<string, Composition>();
    const writes = new Map<string, Promise<void>>();
    const instructions = new Map<string, Promise<string | undefined>>();
    // Registration is capability-gated: without the daemon endpoint this
    // plugin stays instruction-and-composition only, exactly as before the
    // tool existed. A missing registration is also what keeps the tool out
    // of every snapshot; the per-request hook below is the second gate.
    let checkRegistration: Registration | undefined;
    if (checkAddress && checkToken) {
      checkRegistration = await ctx.tool.transform(editor => editor.add(mermaidCheckTool));
      const registered = await ctx.tool.list();
      if (!registered.some(entry => entry.id === MERMAID_CHECK_TOOL)) {
        throw new Error("OpenCode did not register the Fintwind mermaid check tool.");
      }
    }
    // First-token latency, measured inside the server: `http.request` opens
    // the provider call, `http.response` fires when the stream opens, so the
    // difference is the provider's own time to first byte for that request.
    // Only the agent loop's primary requests count; compaction, title, and
    // generate calls carry other kinds. WebSocket-streaming providers bypass
    // these hooks, so those sessions report nothing and the desktop falls
    // back to its own wall-clock sample.
    //
    // The sample rides plugin storage, not a closure Map: OpenCode can
    // re-setup this plugin while the earlier instance's hooks still receive
    // events, and storage is the one state every instance shares.
    const isPrimary = (event: HttpRequestEvent) =>
      typeof event.sessionID === "string" && (event.sessionID as string).length > 0 && event.kind === "primary";
    const ttftKey = (sessionID: string) => `ttft:${sessionID}`;
    let timingHooks: Registration | undefined;
    try {
      const requestHook = await ctx.session.hook<HttpRequestEvent>("http.request", event => {
        if (isPrimary(event)) {
          void ctx.storage.set(`ttft-request:${event.sessionID as string}`, Date.now()).catch(() => {});
        }
      });
      const responseHook = await ctx.session.hook<HttpRequestEvent>("http.response", async event => {
        if (!isPrimary(event)) return;
        const sessionID = event.sessionID as string;
        const requestKey = `ttft-request:${sessionID}`;
        const started = await ctx.storage.get(requestKey).catch(() => undefined);
        if (typeof started !== "number") return;
        void ctx.storage.remove(requestKey).catch(() => {});
        await ctx.storage.set(ttftKey(sessionID), Math.max(0, Date.now() - started)).catch(() => {});
      });
      timingHooks = { async dispose() { await requestHook.dispose(); await responseHook.dispose(); } };
    } catch {}
    function instruction(sessionID: string): Promise<string | undefined> {
      const saved = instructions.get(sessionID);
      if (saved) return saved;
      const pending = ctx.session.get({ sessionID }).then(raw => {
        const info = object(raw);
        if (!info) throw new Error("Session metadata is unavailable");
        const version = object(info.metadata)?.fintwindMermaidPrompt;
        if (version === undefined || version === null) return undefined;
        if (version === "v1") return MERMAID_PROMPT_V1;
        if (version === "v2") return MERMAID_PROMPT_V2;
        if (version === "v3") return MERMAID_PROMPT_V3;
        if (version === "v4") return MERMAID_PROMPT_V4;
        throw new Error("Unsupported Mermaid prompt version");
      });
      instructions.set(sessionID, pending);
      // Fail the request, not silently its instructions. A later request can
      // retry metadata resolution without ever dispatching a different prefix.
      void pending.catch(() => {
        if (instructions.get(sessionID) === pending) instructions.delete(sessionID);
      });
      return pending;
    }
    const hook = await ctx.session.hook<ContextEvent>("context", async event => {
      const text = await instruction(event.sessionID);
      if (text !== undefined) {
        if (!Array.isArray(event.system)) throw new Error("Missing model system context");
        if (!event.system.some(part => object(part)?.type === "text" && object(part)?.text === text)) {
          event.system.push({ type: "text", text });
        }
      }
      // The check tool belongs only to sessions that render diagrams: the
      // same metadata snapshot that adds the instruction gates the tool, so
      // other sessions never carry it in their tool snapshot.
      if (text === undefined && event.tools && typeof event.tools === "object") {
        delete (event.tools as Record<string, unknown>)[MERMAID_CHECK_TOOL];
      }
      try {
        const sources = contextSources(await ctx.session.context({ sessionID: event.sessionID }));
        const result = composition(event, sources);
        if (!result || !event.sessionID) return;
        latest.set(event.sessionID, result);
        const previous = writes.get(event.sessionID) ?? Promise.resolve();
        const pending = previous
          .catch(() => {})
          .then(() => ctx.storage.set(`context:${event.sessionID}`, result));
        writes.set(event.sessionID, pending);
        void pending.catch(() => {});
      } catch {}
    });
    const rpc = await ctx.rpc.register(definition, {
      async get(raw: unknown) {
        const request = object(raw);
        const id = request?.sessionID;
        if (typeof id !== "string" || !id) throw new Error("Missing sessionID");
        const cached = latest.get(id);
        if (cached) return cached;
        const result = parseComposition(await ctx.storage.get(`context:${id}`));
        if (!result) return null;
        latest.set(id, result);
        return result;
      },
      // Consume-on-read: the desktop asks once per turn's first settled step,
      // so a stored sample always belongs to the request that just finished.
      // An empty answer means the provider path bypassed the HTTP hooks (or
      // the hooks are unavailable on this OpenCode build).
      async ttft(raw: unknown) {
        const request = object(raw);
        const id = request?.sessionID;
        if (typeof id !== "string" || !id) throw new Error("Missing sessionID");
        const value = await ctx.storage.get(ttftKey(id)).catch(() => undefined);
        void ctx.storage.remove(ttftKey(id)).catch(() => {});
        return typeof value === "number" ? value : null;
      },
    });
    return async () => {
      await hook.dispose();
      await rpc.dispose();
      await checkRegistration?.dispose().catch(() => {});
      await timingHooks?.dispose().catch(() => {});
      await Promise.all([...writes.values()].map(write => write.catch(() => {})));
      writes.clear();
      latest.clear();
      instructions.clear();
    };
  },
};
