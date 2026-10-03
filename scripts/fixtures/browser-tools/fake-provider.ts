/**
 * Phase-three E2E fixture: a deterministic, loopback-only OpenAI-compatible
 * "model provider" that Fintwind's private OpenCode server is pointed at.
 *
 * Failure modes this fixture exists to surface, and how:
 * - **No external provider**: it binds 127.0.0.1 and answers only local
 *   requests, so the run can never send a real prompt to a paid account.
 * - **Non-deterministic tool flow**: every response is a fixed function of the
 *   ordered plan the runner installs and the tool results observed so far, so
 *   the same run produces the same provider-call count and the same tool call
 *   sequence every time.
 * - **Silently wrong tool payloads**: it builds every tool's arguments from
 *   the real `fintwind_browser_list` result (pageId/grantId) or an explicit
 *   override, so a tool call can only name a page the model actually saw (or
 *   one the runner deliberately injects to probe scope).
 * - **Hidden model contract**: the exact bytes the plugin returns to the model
 *   (including the untrusted-source wrapper) are recorded verbatim for the
 *   runner to assert on, rather than being hand-checked.
 *
 * It speaks OpenAI chat-completions SSE and a `/models` listing. It exposes a
 * control surface the runner uses in-process (this runs in the same Bun
 * process as the runner): install a plan, read the provider-call count, and
 * read the recorded tool results.
 *
 * The plugin's own tool names and the built-in browser tool ids are imported
 * from the plugin source, so a rename can never silently make this fixture
 * assert against a tool that no longer exists.
 */

import { BROWSER_INSTRUCTION_MARKER } from '../../../resources/opencode-browser-plugin.ts';

/** One recorded tool result. For the common string content, `raw` is the
 *  exact `content` string OpenCode handed back to the model. For array content
 *  (the screenshot path) `raw` is only the `{type:"text"}` part's wrapper
 *  string — never the stringified array — and the `{type:"file"}` part is
 *  captured in the three screenshot fields. */
export type RecordedToolResult = {
  /** The tool the turn was executing when this result came back. */
  tool: string;
  /** The raw `content` string of the tool message; for array content, the
   *  text part's string only. */
  raw: string;
  /** `raw` parsed as JSON when possible, else null. */
  parsed: unknown;
  /** Whether the plugin tagged the result as an untrusted source. */
  source: string | null;
  /** The `value` the plugin wrapped, when `source` was present. */
  value: unknown;
  /** When the result looks like a browser error result. */
  browserError: string | null;
  /** When the content was an array with a `{type:"file"}` part: its `mime`
   *  (e.g. "image/png"), else null. */
  screenshotMime: string | null;
  /** The file part's `name` (e.g. "screenshot.png"), else null. */
  screenshotName: string | null;
  /** The file part's `uri` — the `data:<mime>;base64,<data>` URL the model
   *  sees the image through — else null. */
  screenshotDataUri: string | null;
};

/** Where the tool's pageId/grantId come from. */
type PageSource = { from: 'list'; index?: number } | { pageId: string; grantId: string };

/** One element a snapshot observed. Prefer the opaque ref; a CSS selector is
 *  the legacy fallback. Exactly one is ever set. */
export type ElementTarget =
  | { ref: string; selector?: undefined }
  | { selector: string; ref?: undefined };

/** A single step of the deterministic plan the provider emits. The page
 *  address comes from the last list result (or an explicit override), and an
 *  element step carries the same `ref`/`selector` target shape the model is
 *  told to use. */
export type PlanStep =
  | { tool: 'fintwind_browser_open'; url: string }
  | { tool: 'fintwind_browser_list' }
  | { tool: 'fintwind_browser_snapshot'; page: PageSource }
  | { tool: 'fintwind_browser_click'; page: PageSource; target: ElementTarget }
  | { tool: 'fintwind_browser_fill'; page: PageSource; target: ElementTarget; text: string }
  | { tool: 'fintwind_browser_scroll'; page: PageSource; deltaY: number }
  | { tool: 'fintwind_browser_navigate'; page: PageSource; url: string }
  | { tool: 'fintwind_browser_screenshot'; page: PageSource; fullPage?: boolean }
  | { tool: 'fintwind_browser_evaluate'; page: PageSource; expression: string }
  | { tool: 'fintwind_browser_press'; page: PageSource; target: ElementTarget; key: string }
  | { tool: 'fintwind_browser_select'; page: PageSource; target: ElementTarget; value: string }
  | { tool: 'fintwind_browser_hover'; page: PageSource; target: ElementTarget }
  | { tool: 'fintwind_browser_double_click'; page: PageSource; target: ElementTarget }
  | { tool: 'fintwind_browser_drag'; page: PageSource; from: string; to: string }
  | { tool: 'fintwind_browser_click_at'; page: PageSource; x: number; y: number }
  | { tool: 'fintwind_browser_close'; page: PageSource };

const MODEL_ID = 'fixture-model';
const FINAL_TEXT = 'FIXTURE-DONE';

/** A minimal provider request, captured for the run timeline (ids redacted size). */
type CapturedRequest = {
  messageCount: number;
  lastRole: string;
  hasToolResults: boolean;
  /** Whether the armed turn marker is in this request's messages, independent
   *  of tool presence. */
  carriesMarker: boolean;
  real: boolean;
  /** Names of the tools OpenCode declared in this request (what the model could call). */
  toolNames: string[];
  /** Whether the Fintwind browser instruction reached this request. Only a
   *  flag is kept: the prompt itself is never written to an artifact. */
  fintwindBrowserInstruction: boolean;
};

export class FakeProvider {
  private plan: PlanStep[] = [];
  private cursor = 0;
  /** The exact marker string that identifies the turn currently under test. A
   *  request that does not carry it (a title summary, a warmup) is auxiliary
   *  and answered with terminal text, so it can never consume a plan step. */
  private marker = '';
  calls = 0;
  turnCalls = 0;
  toolCalls = 0;
  toolResults: RecordedToolResult[] = [];
  requests: CapturedRequest[] = [];
  lastError: string | null = null;

  /** Arm the provider for one turn: install `plan`, tag it with `marker`, and
   *  reset the per-turn counters and recorded results. */
  plan_install(plan: PlanStep[], marker: string): void {
    this.plan = plan;
    this.marker = marker;
    this.cursor = 0;
    this.turnCalls = 0;
    this.toolResults = [];
  }

  /** Turn the provider back to auxiliary mode (terminal text, no tool calls). */
  plan_disarm(): void {
    this.marker = '';
    this.plan = [];
    this.cursor = 0;
  }

  /** Total provider invocations across the whole run. */
  count(): number {
    return this.calls;
  }

  private lastListPages(): Array<{ pageId: string; grantId: string }> {
    for (let index = this.toolResults.length - 1; index >= 0; index -= 1) {
      const result = this.toolResults[index]!;
      if (result.tool === 'fintwind_browser_list' && Array.isArray(result.value)) {
        return result.value as Array<{ pageId: string; grantId: string }>;
      }
    }
    return [];
  }

  private pageIds(page: PageSource): { pageId: string; grantId: string } {
    if ('from' in page) {
      const pages = this.lastListPages();
      const entry = pages[page.index ?? 0];
      if (!entry) {
        throw new Error(`fake provider has no listed page at index ${page.index ?? 0}`);
      }
      return { pageId: entry.pageId, grantId: entry.grantId };
    }
    return { pageId: page.pageId, grantId: page.grantId };
  }

  toolArguments(step: PlanStep): Record<string, unknown> {
    switch (step.tool) {
      case 'fintwind_browser_open':
        return { url: step.url };
      case 'fintwind_browser_list':
        return {};
      case 'fintwind_browser_snapshot':
        return this.pageIds(step.page);
      case 'fintwind_browser_click':
        return { ...this.pageIds(step.page), ...step.target };
      case 'fintwind_browser_fill':
        return { ...this.pageIds(step.page), ...step.target, text: step.text };
      case 'fintwind_browser_scroll':
        return { ...this.pageIds(step.page), deltaY: step.deltaY };
      case 'fintwind_browser_navigate':
        return { ...this.pageIds(step.page), url: step.url };
      case 'fintwind_browser_screenshot':
        return { ...this.pageIds(step.page), fullPage: step.fullPage === true };
      case 'fintwind_browser_evaluate':
        return { ...this.pageIds(step.page), expression: step.expression };
      case 'fintwind_browser_press':
        return { ...this.pageIds(step.page), ...step.target, key: step.key };
      case 'fintwind_browser_select':
        return { ...this.pageIds(step.page), ...step.target, value: step.value };
      case 'fintwind_browser_hover':
      case 'fintwind_browser_double_click':
        return { ...this.pageIds(step.page), ...step.target };
      case 'fintwind_browser_drag':
        return { ...this.pageIds(step.page), from: step.from, to: step.to };
      case 'fintwind_browser_click_at':
        return { ...this.pageIds(step.page), x: step.x, y: step.y };
      case 'fintwind_browser_close':
        return this.pageIds(step.page);
    }
  }

  /** The most recent assistant tool call name before the trailing tool result. */
  private lastToolName(messages: Array<Record<string, unknown>>): string {
    for (let index = messages.length - 1; index >= 0; index -= 1) {
      const message = messages[index]!;
      if (message.role === 'assistant' && Array.isArray(message.tool_calls)) {
        const calls = message.tool_calls as Array<{ function?: { name?: string } }>;
        const last = calls[calls.length - 1];
        if (last?.function?.name) return last.function.name;
      }
    }
    return 'unknown';
  }

  /** Decide the provider response for one OpenAI chat-completions request. */
  decide(
    body: { model?: string; messages?: Array<Record<string, unknown>>; tools?: unknown[]; system?: unknown },
  ): { tool: PlanStep; id: string } | { final: true } {
    this.calls += 1;
    const messages = Array.isArray(body.messages) ? body.messages : [];
    const last = messages[messages.length - 1];
    const toolNames = Array.isArray(body.tools)
      ? body.tools.map(entry => {
          const record = entry as { function?: { name?: string }; name?: string };
          return record.function?.name ?? record.name ?? '';
        }).filter(name => name.length > 0)
      : [];
    // Only a request that carries the armed turn marker is the turn under
    // test; anything else (a title summary, a catalog warmup) is auxiliary and
    // must not touch the plan, the cursor, or the recorded results.
    const serialized = safeStringify(messages);
    // The turn under test is the agent context turn: it carries the armed
    // marker AND declares a tool set (OpenCode's title/summary/generate
    // auxiliary requests carry no tools, per the plugin docs, so gating on
    // tools.length>0 keeps them from consuming a plan step).
    const real = this.marker !== '' && serialized.includes(this.marker) && toolNames.length > 0;
    // The instruction is looked for in the whole envelope, because a provider
    // may receive the system prompt either as a message or as its own field.
    const instruction = serialized.includes(BROWSER_INSTRUCTION_MARKER)
      || safeStringify(body.system).includes(BROWSER_INSTRUCTION_MARKER);
    this.requests.push({
      messageCount: messages.length,
      lastRole: typeof last?.role === 'string' ? last.role : 'none',
      hasToolResults: messages.some(message => message.role === 'tool'),
      // Independent of `real`: the marker identifies the request under test
      // even when a disabled plugin leaves the request with no tools at all.
      carriesMarker: serialized.includes(this.marker),
      real,
      toolNames,
      fintwindBrowserInstruction: instruction,
    });
    if (!real) {
      return { final: true };
    }
    this.turnCalls += 1;

    if (last && last.role === 'tool') {
      this.record_tool_result(messages);
      this.cursor += 1;
      if (this.cursor < this.plan.length) {
        const step = this.plan[this.cursor]!;
        this.toolCalls += 1;
        return { tool: step, id: `call_${this.toolCalls}` };
      }
      return { final: true };
    }

    // OpenCode extracts media from a tool result into a synthetic user
    // message for providers that cannot carry images inside tool results:
    // v2.0.16 serializes it as a bare content array of `image_url` parts
    // with data: URIs (no explanatory text). That message is the completion
    // of the step this fixture just issued, so it advances the plan exactly
    // like a tool message would — treating it as a fresh turn would restart
    // the plan and loop the same call forever.
    if (last && last.role === 'user' && isMediaExtractionMessage(last)) {
      this.record_media_extraction(messages, serialized);
      this.cursor += 1;
      if (this.cursor < this.plan.length) {
        const step = this.plan[this.cursor]!;
        this.toolCalls += 1;
        return { tool: step, id: `call_${this.toolCalls}` };
      }
      return { final: true };
    }
    if (last && last.role === 'user' && process.env.E2E_DEBUG_USER_MESSAGES === '1') {
      console.error('[e2e-debug] user message content:', safeStringify(last).slice(0, 800));
    }

    // A fresh turn message begins the plan from the top.
    this.cursor = 0;
    if (this.plan.length === 0) {
      return { final: true };
    }
    const step = this.plan[0]!;
    this.toolCalls += 1;
    return { tool: step, id: `call_${this.toolCalls}` };
  }

  private record_tool_result(messages: Array<Record<string, unknown>>): void {
    const last = messages[messages.length - 1]!;
    // Array content is the screenshot path: the `{type:"text"}` part carries
    // the untrusted wrapper JSON and the `{type:"file"}` part carries the
    // data-URI image. The recorded `raw` is the text part's string, parsed as
    // JSON — never the stringified array, which no model-side consumer sees.
    // A plain string content result keeps its original shape unchanged.
    const parts = Array.isArray(last.content)
      ? last.content as Array<Record<string, unknown>>
      : undefined;
    const textPart = parts?.find(part => part?.type === 'text');
    const filePart = parts?.find(part => part?.type === 'file');
    const raw = parts
      ? (typeof textPart?.text === 'string' ? textPart.text : '')
      : (typeof last.content === 'string' ? last.content : JSON.stringify(last.content ?? ''));
    let parsed: unknown = null;
    try {
      parsed = JSON.parse(raw);
    } catch {
      parsed = null;
    }
    const wrapper =
      parsed && typeof parsed === 'object'
        ? (parsed as { source?: unknown; value?: unknown })
        : {};
    const source = typeof wrapper.source === 'string' ? wrapper.source : null;
    const value =
      parsed && typeof parsed === 'object' && 'value' in (parsed as object)
        ? wrapper.value
        : undefined;
    // A browser error result reaches the model wrapped too, but its value is
    // `{kind:"error",message}` after the plugin surfaces the thrown error... in
    // fact the plugin rejects and OpenCode records the tool error text, which
    // is NOT JSON-wrapped. So a non-wrapped string is a browser/tool error.
    let browserError: string | null = null;
    if (source === null) {
      browserError = raw.slice(0, 400);
    } else if (
      value &&
      typeof value === 'object' &&
      (value as { kind?: string }).kind === 'error'
    ) {
      browserError = String((value as { message?: unknown }).message ?? 'error').slice(0, 400);
    }
    this.toolResults.push({
      tool: this.lastToolName(messages),
      raw,
      parsed,
      source,
      value,
      browserError,
      screenshotMime: filePart && typeof filePart.mime === 'string' ? filePart.mime : null,
      screenshotName: filePart && typeof filePart.name === 'string' ? filePart.name : null,
      screenshotDataUri: filePart && typeof filePart.uri === 'string' ? filePart.uri : null,
    });
  }

  /** Record a tool step whose media OpenCode moved into a synthetic user
   *  message. The untrusted text wrapper stays behind in the tool message —
   *  the extraction moves only the file parts — so the wrapper is read from
   *  the last tool message, and the image is recovered from the serialized
   *  user message, where the OpenAI-compatible shape carries it as a
   *  data-URI `image_url` (a shape this fixture must not over-specify). */
  private record_media_extraction(messages: Array<Record<string, unknown>>, serialized: string): void {
    const step = this.plan[this.cursor];
    if (!step) return;
    let raw = '';
    for (let index = messages.length - 1; index >= 0; index -= 1) {
      const message = messages[index]!;
      if (message.role === 'tool' && typeof message.content === 'string') {
        raw = message.content;
        break;
      }
    }
    let source: string | null = null;
    try {
      const parsed = JSON.parse(raw) as { source?: unknown };
      if (typeof parsed.source === 'string') source = parsed.source;
    } catch {
      /* An unparseable wrapper records as null; the data URI is the evidence. */
    }
    const dataUri = /data:image\/[a-z+]+;base64,[A-Za-z0-9+/=]+/.exec(serialized)?.[0] ?? null;
    const mime = dataUri?.slice(5, dataUri.indexOf(';')) ?? null;
    this.toolResults.push({
      tool: step.tool,
      raw,
      parsed: null,
      source,
      value: undefined,
      browserError: null,
      screenshotMime: mime,
      screenshotName: null,
      screenshotDataUri: dataUri,
    });
  }
}

/** Serialize one tool-call decision into an OpenAI streaming SSE body. */
function toolCallSse(id: string, name: string, argsJson: string): string {
  const base = { id: `chatcmpl-${id}`, object: 'chat.completion.chunk', created: 0, model: MODEL_ID };
  const call = {
    ...base,
    choices: [
      {
        index: 0,
        delta: {
          role: 'assistant',
          tool_calls: [
            { index: 0, id, type: 'function', function: { name, arguments: argsJson } },
          ],
        },
        finish_reason: null,
      },
    ],
  };
  const done = {
    ...base,
    choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }],
    usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
  };
  return `data: ${JSON.stringify(call)}\n\ndata: ${JSON.stringify(done)}\n\ndata: [DONE]\n\n`;
}

/** Serialize a final text decision into an OpenAI streaming SSE body. */
function finalSse(text: string): string {
  const base = { id: 'chatcmpl-final', object: 'chat.completion.chunk', created: 0, model: MODEL_ID };
  const content = {
    ...base,
    choices: [{ index: 0, delta: { role: 'assistant', content: text }, finish_reason: null }],
  };
  const done = {
    ...base,
    choices: [{ index: 0, delta: {}, finish_reason: 'stop' }],
    usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
  };
  return `data: ${JSON.stringify(content)}\n\ndata: ${JSON.stringify(done)}\n\ndata: [DONE]\n\n`;
}

/**
 * Start the loopback fake provider and return its base URL plus a handle the
 * runner reads directly. `FakeProvider` holds all state in memory, so there is
 * no separate control protocol to leak into an artifact.
 */
export function startFakeProvider(): { baseUrl: string; provider: FakeProvider; stop: () => void } {
  const provider = new FakeProvider();
  const server = Bun.serve({
    hostname: '127.0.0.1',
    port: 0,
    fetch(request: Request): Response | Promise<Response> {
      const url = new URL(request.url);
      if (url.pathname === '/v1/models' && request.method === 'GET') {
        return Response.json({
          object: 'list',
          data: [{ id: MODEL_ID, object: 'model', created: 0, owned_by: 'fintwind-e2e' }],
        });
      }
      if (url.pathname === '/v1/chat/completions' && request.method === 'POST') {
        return request
          .json()
          .then(body => {
            let decision: { tool: PlanStep; id: string } | { final: true };
            try {
              decision = provider.decide(body as { model?: string; messages?: Array<Record<string, unknown>> });
            } catch (error) {
              provider.lastError = error instanceof Error ? error.message : String(error);
              // A fixture bug must not masquerade as a provider error; answer a
              // terminal text so the turn ends and the runner sees the failure.
              return sseResponse(finalSse(`FIXTURE-ERROR: ${provider.lastError}`));
            }
            if ('final' in decision) {
              return sseResponse(finalSse(FINAL_TEXT));
            }
            let argsJson: string;
            try {
              argsJson = JSON.stringify(provider.toolArguments(decision.tool));
            } catch (error) {
              provider.lastError = error instanceof Error ? error.message : String(error);
              return sseResponse(finalSse(`FIXTURE-ERROR: ${provider.lastError}`));
            }
            return sseResponse(toolCallSse(decision.id, decision.tool.tool, argsJson));
          })
          .catch(error => {
            provider.lastError = error instanceof Error ? error.message : String(error);
            return new Response(`fixture error: ${provider.lastError}`, { status: 400 });
          });
      }
      return new Response('not found', { status: 404 });
    },
  });
  const baseUrl = `http://127.0.0.1:${server.port}/v1`;
  return { baseUrl, provider, stop: () => server.stop(true) };
}

function sseResponse(body: string): Response {
  return new Response(body, {
    status: 200,
    headers: { 'content-type': 'text/event-stream; charset=utf-8', 'cache-control': 'no-store' },
  });
}

/** Best-effort stringify for marker detection; never throws. */
function safeStringify(value: unknown): string {
  try {
    return JSON.stringify(value) ?? '';
  } catch {
    return '';
  }
}

/** The synthetic user message OpenCode builds when it extracts a tool
 *  result's media for a provider without media-in-tool-result support: a
 *  content array of `image_url` parts carrying data: URIs, and nothing a
 *  human typed. A plain prompt string never matches. */
function isMediaExtractionMessage(message: Record<string, unknown>): boolean {
  if (!Array.isArray(message.content)) return false;
  const parts = message.content as Array<Record<string, unknown>>;
  return parts.some(part => {
    const url = (part?.image_url as { url?: unknown } | undefined)?.url;
    return part?.type === 'image_url' && typeof url === 'string' && url.startsWith('data:');
  });
}
