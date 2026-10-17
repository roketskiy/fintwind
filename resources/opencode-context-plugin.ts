type Registration = { dispose(): Promise<void> };
type ContextEvent = {
  sessionID: string;
  model?: { providerID?: unknown; id?: unknown };
  system?: unknown;
  messages?: unknown;
  tools?: unknown;
};
type Composition = {
  model: string;
  system: number;
  tools: number;
  memory: number;
  messages: number;
  incomplete: boolean;
};
type Context = {
  storage: { get(key: string): Promise<unknown>; set(key: string, value: unknown): Promise<void> };
  session: { hook(kind: "context", handler: (event: ContextEvent) => void): Promise<Registration> };
  rpc: { register(definition: unknown, handlers: { get(input: unknown): Promise<Composition | null> }): Promise<Registration> };
};

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
  if (!saved || typeof saved.model !== "string" || typeof saved.incomplete !== "boolean") return undefined;
  const count = (key: string): number | undefined => {
    const value = saved[key];
    return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : undefined;
  };
  const system = count("system");
  const tools = count("tools");
  const memory = count("memory");
  const messages = count("messages");
  if (system === undefined || tools === undefined || memory === undefined || messages === undefined) return undefined;
  return { model: saved.model, system, tools, memory, messages, incomplete: saved.incomplete };
}

function resultContent(value: unknown, counts: Composition): number {
  const result = object(value);
  if (!result || typeof result.type !== "string") { counts.incomplete = true; return 0; }
  if (result.type !== "content") return estimateValue(result.value);
  if (!Array.isArray(result.value)) { counts.incomplete = true; return 0; }
  let estimate = 0;
  for (const item of result.value) {
    const content = object(item);
    if (content?.type === "text" && typeof content.text === "string") estimate += estimateText(content.text);
    else counts.incomplete = true;
  }
  return estimate;
}

function composition(event: ContextEvent): Composition | undefined {
  if (!event.sessionID || !Array.isArray(event.system) || !Array.isArray(event.messages)) return undefined;
  const provider = event.model?.providerID;
  const modelId = event.model?.id;
  if (typeof provider !== "string" || typeof modelId !== "string") return undefined;
  const counts = { model: `${provider}/${modelId}`, system: 0, tools: 0, memory: 0, messages: 0, incomplete: false };
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
  for (const rawMessage of event.messages) {
    const message = object(rawMessage);
    if (!message || !Array.isArray(message.content)) { counts.incomplete = true; continue; }
    for (const rawPart of message.content) {
      const part = object(rawPart);
      if (!part || typeof part.type !== "string") { counts.incomplete = true; continue; }
      switch (part.type) {
        case "text":
          if (typeof part.text === "string") {
            const target = message.role === "system" ? "system" : "messages";
            counts[target] += estimateText(part.text);
          } else counts.incomplete = true;
          break;
        case "reasoning":
          if (typeof part.text === "string") counts.messages += estimateText(part.text);
          if (object(part)?.encrypted !== undefined) counts.incomplete = true;
          break;
        case "tool-call":
          counts.tools += estimateValue({ name: part.name, input: part.input });
          break;
        case "tool-result":
          counts.tools += resultContent(part.result, counts);
          break;
        case "compaction":
          if (typeof part.text === "string") counts.memory += estimateText(part.text);
          else counts.incomplete = true;
          if (object(part)?.encrypted !== undefined) counts.incomplete = true;
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
const definition = { id: "fintwind.context", methods: { get: { input, output: {} } }, events: {} };

export default {
  id: "fintwind.context",
  async setup(ctx: Context) {
    const latest = new Map<string, Composition>();
    const writes = new Map<string, Promise<void>>();
    const hook = await ctx.session.hook("context", event => {
      try {
        const result = composition(event);
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
      async get(raw) {
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
    });
    return async () => {
      await hook.dispose();
      await rpc.dispose();
      await Promise.all([...writes.values()].map(write => write.catch(() => {})));
      writes.clear();
      latest.clear();
    };
  },
};
