/**
 * Minimal, independent daemon-client + stepfun helpers for the native
 * (`browser-tools-native.ts`) E2E. It deliberately does not import the
 * automated-provider runner: this E2E uses the real Step 5 Preview model and a
 * real WebView2 host, so the only shared pieces are the on-the-wire protocol.
 *
 * Failure modes this helper is built to keep honest:
 * - hard-coding the Fintwind app session where the *native* OpenCode session is
 *   meant (the two differ; the native id is the one the transcript reads);
 * - answering a driver command before its response, or hanging forever (every
 *   request is timeout-bounded and the connection is owned for cleanup);
 * - leaking a token: only the daemon's ephemeral token lives here, in memory.
 */
import { randomUUID } from 'node:crypto';

/** The daemon's framed JSON client: only what the native E2E needs. */
export class DaemonSocket {
  private socket!: WebSocket;
  private pending = new Map<string, { resolve: (v: unknown) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  hello: { daemonVersion?: string } | null = null;
  protocolVersion = 0;
  /** Live-only driver events and notifications, newest last. */
  notifications: Array<Record<string, unknown>> = [];

  constructor(readonly address: string, readonly token: string) {
    this.socket = new WebSocket(address);
    this.socket.onopen = () => {};
    this.socket.onmessage = (event: MessageEvent) => {
      let message: { type: string; requestId?: string; outcome?: unknown; daemonVersion?: string };
      try { message = JSON.parse(String(event.data)); } catch { return; }
      if (message.type === 'hello') this.hello = { daemonVersion: message.daemonVersion };
      if (message.type === 'response' && message.requestId && this.pending.has(message.requestId)) {
        const entry = this.pending.get(message.requestId)!;
        this.pending.delete(message.requestId);
        clearTimeout(entry.timer);
        entry.resolve(message.outcome);
        return;
      }
      if (message.type !== 'response') this.notifications.push(message as Record<string, unknown>);
    };
  }

  async connect(timeoutMs = 10_000): Promise<void> {
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`could not reach ${this.address}`)), timeoutMs);
      this.socket.onopen = () => { clearTimeout(timer); resolve(); };
      this.socket.onerror = () => { clearTimeout(timer); reject(new Error(`could not reach ${this.address}`)); };
    });
    this.send({ type: 'hello', protocolVersion: this.protocolVersion, token: this.token, clientId: randomUUID(), resumeFrom: [] });
    const until = Date.now() + timeoutMs;
    while (this.hello === null && Date.now() < until) await Bun.sleep(40);
    if (this.hello === null) throw new Error('the daemon never answered the hello');
  }

  private send(message: unknown) { this.socket.send(JSON.stringify(message)); }

  request(sessionId: string, runtimeId: string, command: unknown, requestId = randomUUID(), timeoutMs = 60_000): Promise<{ status: string; payload?: Record<string, unknown> }> {
    let timer: ReturnType<typeof setTimeout>;
    const outcome = new Promise<{ status: string; payload?: Record<string, unknown> }>((resolve, reject) => {
      timer = setTimeout(() => { this.pending.delete(requestId); reject(new Error(`the daemon did not answer ${requestId} within ${timeoutMs} ms`)); }, timeoutMs);
      this.pending.set(requestId, { resolve: resolve as (v: unknown) => void, reject, timer });
    });
    this.send({ type: 'request', requestId, sessionId, runtimeId, command });
    return outcome;
  }

  control(message: unknown) { this.send(message); }

  close() {
    try { this.socket.close(); } catch { /* Already closed. */ }
  }
}

/** The `Command::Start` options for the native run, on the wire. */
export function wireStartOptions(init: {
  binary: string;
  cwd: string;
  model: string;
  reasoningEffort: string | null;
}): Record<string, unknown> {
  return {
    binary: init.binary,
    cwd: init.cwd,
    mode: 'fullAccess',
    interactionMode: 'build',
    model: init.model,
    // The driver turns this into the model reference's `variant`.
    reasoningEffort: init.reasoningEffort,
    serviceTier: null,
    contextWindow: null,
    agentPreset: null,
    providerCursor: null,
  };
}

/** The native OpenCode session id a started runtime announces in its `connected`
 *  event. This is the id the transcript is read for; it is not the app session. */
export function connectedNativeSession(client: DaemonSocket): string | null {
  for (const note of client.notifications) {
    const event = note.event as { kind?: unknown; payload?: unknown } | undefined;
    if (event?.kind !== 'connected') continue;
    const found = findSessionId(event.payload);
    if (found) return found;
  }
  return null;
}

function findSessionId(value: unknown): string | null {
  if (!value || typeof value !== 'object') return null;
  if (Array.isArray(value)) {
    for (const entry of value) { const found = findSessionId(entry); if (found) return found; }
    return null;
  }
  const record = value as Record<string, unknown>;
  for (const key of ['sessionId', 'session_id', 'sessionID']) {
    const candidate = record[key];
    if (typeof candidate === 'string' && candidate.length > 0) return candidate;
  }
  for (const nested of Object.values(record)) {
    const found = findSessionId(nested);
    if (found) return found;
  }
  return null;
}

/** Stable wait for the native session id, polling the daemon's event feed. */
export async function waitNativeSession(client: DaemonSocket, timeoutMs = 30_000): Promise<string> {
  const until = Date.now() + timeoutMs;
  while (Date.now() < until) {
    const found = connectedNativeSession(client);
    if (found) return found;
    await Bun.sleep(80);
  }
  throw new Error('the daemon never announced the native OpenCode session');
}

const NIL = '00000000-0000-0000-0000-000000000000';

/** Read a native session's transcript and return it as a string for a marker
 *  search. The driver reads only the session's own messages; nothing here
 *  writes to the transcript or touches the credential store. */
export async function fetchTranscriptText(client: DaemonSocket, init: {
  binary: string;
  cwd: string;
  sessionId: string;
}, timeoutMs = 60_000): Promise<string> {
  const outcome = await client.request(NIL, NIL, {
    type: 'fetchNativeTranscript',
    binary: init.binary,
    directory: init.cwd,
    sessionId: init.sessionId,
  }, randomUUID(), timeoutMs);
  if (outcome.status !== 'ok') throw new Error(`fetchNativeTranscript failed: ${JSON.stringify(outcome).slice(0, 300)}`);
  const transcript = outcome.payload?.transcript as { blocks?: unknown; messages?: Array<{ role?: string; content?: string }> } | undefined;
  if (!transcript) throw new Error('Native transcript response has no transcript.');
  // User prompts cannot prove that the provider received page data. Search
  // only the real tool/activity blocks and assistant output, never the prompt.
  const text = JSON.stringify({ blocks: transcript.blocks,
    assistant: transcript.messages?.filter(message => message.role === 'assistant') });
  // A huge transcript is not the point; keep the marker search bounded.
  if (text.length > 2_000_000) return text.slice(0, 2_000_000);
  return text;
}
