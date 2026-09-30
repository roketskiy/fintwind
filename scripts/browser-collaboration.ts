/**
 * Phase-two behavioral E2E for the browser collaboration bridge.
 *
 * Failure modes this runner is built to surface, and what it does about each:
 * - a page that was never shared answering an action;
 * - a scope from another session, another runtime or a superseded grant;
 * - a mutation reaching the page before a human approves it;
 * - a trusted-input regression (synthesized clicks, focus theft from GPUI);
 * - a snapshot that leaks input values, cookie values or an unbounded page;
 * - a cancel or revoke that still mutates, a repeated request id that runs
 *   twice, or a navigation that keeps a lease;
 * - a reconnect that inherits a lease, or a second client answering for a page;
 * - a publish that is refused while the UI still claims the pages are shared;
 * - a run whose build eats its own budget, hangs, leaks a token into a report,
 *   kills a process it does not own, or reports a pass it did not earn.
 *
 * Playwright performs fixture setup, DOM reads and page counting only. Every
 * action under test is performed by the daemon's reverse RPC into the GUI's
 * native CDP adapter, or is refused before it ever reaches the GUI.
 */
import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, type Browser, type Page } from 'playwright-core';
import { startFixture } from './browser-poc-fixture.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
// Every connection in this E2E is loopback. Inherited SOCKS/HTTP proxies must
// not intercept either the local WebSocket or CDP discovery request.
const noProxy = [process.env.NO_PROXY, process.env.no_proxy, '127.0.0.1', 'localhost', '::1'].filter(Boolean).join(',');
process.env.NO_PROXY = noProxy;
process.env.no_proxy = noProxy;
const EXAMPLE_TARGET_DIR = 'target/browser-bridge-e2e';
const POC_TARGET_DIR = 'target/browser-poc';
/** The two cargo builds get their own budget; a cold checkout must not eat the
 *  run budget before the first check even starts. */
const BUILD_LIMIT_MS = 1_200_000;
/** Measured from the moment the builds finish, so a slow compiler cannot turn
 *  a healthy run into a false failure. */
const RUN_LIMIT_MS = 180_000;
/** Below the daemon's 30 s browser timeout, so a stall is an E2E failure. */
const INVOKE_TIMEOUT_MS = 25_000;
const PASSWORD_VALUE = 'p0c-not-a-real-secret';
const HIDDEN_VALUE = 'hidden-not-a-real-secret';
const TEXTAREA_VALUE = 'textarea default not a real secret';
const COOKIE_VALUE = 'p0c-cookie-not-a-real-secret';
const LONG_TEXT = '🌊üñîçøḑé-'.repeat(1200);

const args = new Set(process.argv.slice(2));
for (const arg of args) {
  if (arg !== '--skip-build') throw new Error(`Unknown argument: ${arg}`);
}
if (process.platform !== 'win32') throw new Error('This E2E requires Windows and WebView2.');

const runId = randomUUID();
const output = join(root, 'target', 'browser-bridge-e2e', 'runs', runId);
await mkdir(output, { recursive: true });
const report = {
  runId,
  status: 'running',
  startedAt: new Date().toISOString(),
  finishedAt: '',
  versions: { bun: Bun.version, playwright: '', webview2: '', protocol: 0, hostSha256: '', exampleSha256: '' },
  checks: [] as Array<{ name: string; status: 'passed' | 'failed' | 'skipped'; details: string; durationMs: number }>,
  errors: [] as string[],
  startup: [] as Array<{ step: string; status: 'ok' | 'failed'; details: string }>,
  cleanup: [] as Array<{ step: string; status: 'ok' | 'failed'; details: string }>,
  artifacts: { hostState: join(output, 'host-state.json'), exampleStdout: join(output, 'example.stdout.log') },
};
await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2));

type PageId = 'alpha' | 'beta' | 'gamma';
const PAGE_IDS: PageId[] = ['alpha', 'beta', 'gamma'];
type Scope = { sessionId: string; runtimeId: string; pageId: string; grantId: string };
type BrowserAction =
  | { kind: 'snapshot' }
  | { kind: 'click'; selector: string }
  | { kind: 'fill'; selector: string; text: string }
  | { kind: 'navigate'; url: string };
/** The wire shape of `BrowserResult`: tagged by `kind`, never `status`. */
type InvokeOutcome = { kind: 'ok'; value: unknown } | { kind: 'error'; message: string };

type HostState = {
  runId: string;
  pid: number;
  gpuiFocused: boolean;
  fatal: string | null;
  lastControlId: string | null;
  browserPages: Array<{ page: PageId; pageId: string }>;
  browserShares: Array<{ page: PageId; scope: Scope; url: string; title: string }>;
  pendingBrowserRequests: Array<{ page: PageId; requestId: string; action: string; detail: string }>;
  bridge: { configured: boolean; connected: boolean; sessionId: string | null; runtimeId: string | null; error: string | null } | null;
  pages: Array<{ id: PageId; ready: boolean; url: string | null; title: string | null; nativeFocused: boolean; nativeFocusGains: number; error: string | null }>;
};

let example: ChildProcess | undefined;
let host: ChildProcess | undefined;
let browser: Browser | undefined;
let fixture: ReturnType<typeof startFixture> | undefined;
let daemonAddress = '';
let daemonToken = '';
let daemonProtocolVersion = 0;
/** The bridge daemon's ready line, in scope for `finally` to shut it down. */
let ready: { address: string; token: string; protocolVersion: number } | undefined;
let budgetStartedAt = 0;
let interrupted = false;
const interrupt = () => { interrupted = true; };
process.on('SIGINT', interrupt);
process.on('SIGTERM', interrupt);

/** Every socket this runner opened, so cleanup leaves none behind. */
const ownedSockets = new Set<DaemonSocket>();
/** Every promise this runner started, so none can reject unhandled. */
const inflight: Array<{ label: string; promise: Promise<unknown> }> = [];
function track<T>(label: string, promise: Promise<T>): Promise<T> {
  // Both the caller's promise and this derived guard get handlers, so a check
  // that fails while another is still awaiting cannot end the run with an
  // unhandled rejection and no report.
  const guarded = promise.then(
    value => value,
    error => { throw error; },
  );
  guarded.catch(() => {});
  inflight.push({ label, promise: guarded as Promise<unknown> });
  return guarded;
}

async function remainingMs(): Promise<number> {
  if (!budgetStartedAt) return RUN_LIMIT_MS;
  const left = RUN_LIMIT_MS - (Date.now() - budgetStartedAt);
  if (left <= 5_000) throw new Error('the run exhausted its time budget');
  return left;
}
async function deadline<T>(promise: Promise<T>, label: string, ms: number): Promise<T> {
  if (interrupted) throw new Error('E2E interrupted.');
  let timer: ReturnType<typeof setTimeout>;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error(`${label} timed out`)), ms); }),
    ]);
  } finally { clearTimeout(timer!); }
}
async function poll<T>(label: string, read: () => Promise<T>, accept: (value: T) => boolean, ms = 15_000): Promise<T> {
  const budget = Math.min(ms, await remainingMs());
  const until = Date.now() + budget;
  while (Date.now() < until) {
    if (interrupted) throw new Error('E2E interrupted.');
    const value = await deadline(read(), label, 3_000);
    if (accept(value)) return value;
    await Bun.sleep(120);
  }
  throw new Error(`${label} timed out`);
}
async function state(): Promise<HostState | undefined> {
  try { return JSON.parse(await readFile(join(output, 'host-state.json'), 'utf8')) as HostState; }
  catch (error) {
    if (error instanceof SyntaxError || (error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw error;
  }
}
async function requireState(): Promise<HostState> {
  const value = await state();
  assert(value && value.runId === runId, 'Host state belongs to this run');
  assert(value.pid === host?.pid, 'Host state belongs to the spawned process');
  return value;
}
async function control(action: string, extra: Record<string, string | undefined> = {}) {
  const requestId = randomUUID();
  const path = join(output, 'control.json');
  await writeFile(`${path}.tmp`, JSON.stringify({ requestId, action, ...extra }));
  await rename(`${path}.tmp`, path);
  await poll(`Host ${action}`, requireState, value => value.lastControlId === requestId);
  if (action === 'share-page' && extra.grantId && client) {
    await poll('the daemon accepted the explicit share', async () => {
      const result = await client!.request(sessionId, extra.runtimeId ?? activeRuntimeId, { type: 'browserList' });
      return result.payload?.value as Array<{scope: Scope}>;
    }, shares => shares.some(share => share.scope.grantId === extra.grantId));
  }
  return requestId;
}
async function check(name: string, run: () => Promise<string | void>) {
  const start = performance.now();
  try {
    await remainingMs();
    const details = await deadline(run(), name, Math.max(5_000, await remainingMs()));
    report.checks.push({ name, status: 'passed', details: details ?? 'Verified against the live host and daemon.', durationMs: Math.round(performance.now() - start) });
    console.log(`[OK] ${name}`);
  } catch (error) {
    const details = error instanceof Error ? error.message : String(error);
    report.checks.push({ name, status: 'failed', details, durationMs: Math.round(performance.now() - start) });
    console.error(`[X] ${name}: ${details}`);
    // Retain the host's own view of the world at the moment of the failure.
    try { await writeFile(join(output, `failure-${report.checks.length}.json`), JSON.stringify({ host: await state() }, null, 2)); }
    catch { /* The host may have exited; the check message still stands. */ }
    // Diagnostic only: the same read-only script cannot replace a timed-out
    // native result or turn this failed check into a pass.
    if (details.includes('Runtime.evaluate timed out') && pages.alpha && !pages.alpha.isClosed()) {
      let diagnostic: Record<string, unknown>;
      try {
        const source = await readFile(join(root, 'src', 'browser', 'collaboration.rs'), 'utf8');
        const expression = source.match(/const SNAPSHOT: &str = r#"([\s\S]*?)"#;/)?.[1];
        assert(expression);
        const began = performance.now();
        const result = await deadline(pages.alpha.evaluate(expression), 'renderer snapshot diagnostic', 3000);
        diagnostic = { rendererReturned: true, durationMs: performance.now() - began,
          resultBytes: new TextEncoder().encode(JSON.stringify(result)).length };
        const session = await pages.alpha.context().newCDPSession(pages.alpha);
        try {
          const tree = await session.send('Page.getFrameTree');
          const world = await session.send('Page.createIsolatedWorld', { frameId: tree.frameTree.frame.id,
            worldName: 'fintwind-browser-collaboration', grantUniveralAccess: false });
          const isolatedBegan = performance.now();
          diagnostic.isolatedContextId = world.executionContextId;
          const isolated = await deadline(session.send('Runtime.evaluate', { contextId: world.executionContextId,
            expression, returnByValue: true }), 'isolated renderer snapshot diagnostic', 3000);
          diagnostic.isolatedDurationMs = performance.now() - isolatedBegan;
          diagnostic.isolatedResultBytes = new TextEncoder().encode(JSON.stringify(isolated)).length;
        } catch (error) {
          diagnostic.isolatedError = String(error);
        } finally { await session.detach(); }
      } catch (diagnosticError) {
        diagnostic = { rendererReturned: false, error: String(diagnosticError) };
      }
      await writeFile(join(output, `snapshot-diagnostic-${report.checks.length}.json`), JSON.stringify(diagnostic, null, 2));
    }
  }
  await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2));
}
function record(list: 'startup' | 'cleanup', step: string, error: unknown) {
  const details = error instanceof Error ? error.message : String(error);
  (report[list] as Array<{ step: string; status: string; details: string }>).push({ step, status: 'failed', details });
  console.error(`[X] ${list} ${step}: ${details}`);
}
function exit(child: ChildProcess): Promise<number | null> {
  return new Promise((ok, fail) => { child.once('error', fail); child.once('exit', ok); });
}
async function unusedPort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((ok, fail) => { server.once('error', fail); server.listen(0, '127.0.0.1', ok); });
  const address = server.address();
  assert(address && typeof address !== 'string');
  const port = address.port;
  await new Promise<void>((ok, fail) => server.close(error => error ? fail(error) : ok()));
  return port;
}
function openSocket(): DaemonSocket {
  const socket = new DaemonSocket(daemonAddress, daemonToken);
  ownedSockets.add(socket);
  return socket;
}

/** One WebSocket client with the daemon's framing: tagged, camelCase JSON. */
class DaemonSocket {
  private socket!: WebSocket;
  private pending = new Map<string, { resolve: (value: unknown) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  hello: { type: string } | null = null;
  /** The protocol the daemon printed; this socket answers with the same one. */
  protocolVersion = 0;
  /** Live-only deliveries that are not responses: browser requests, cancels,
   *  revocations and refusals. */
  notifications: Array<{ type: string; [key: string]: unknown }> = [];

  constructor(readonly address: string, readonly token: string) {
    this.socket = new WebSocket(address);
    // The constructor's handler is replaced by `connect`, which sends the
    // hello once it knows the daemon's protocol version.
    this.socket.onopen = () => {};
    this.socket.onmessage = (event: MessageEvent) => {
      let message: { type: string; requestId?: string; outcome?: unknown };
      try { message = JSON.parse(String(event.data)); } catch { return; }
      if (message.type === 'hello') this.hello = { type: message.type };
      if (message.type === 'response' && message.requestId && this.pending.has(message.requestId)) {
        const entry = this.pending.get(message.requestId)!;
        this.pending.delete(message.requestId);
        clearTimeout(entry.timer);
        entry.resolve(message.outcome);
        return;
      }
      if (message.type !== 'response') this.notifications.push(message as { type: string; [key: string]: unknown });
    };
  }

  async connect(timeoutMs = 10_000) {
    const opened = new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`could not reach ${this.address}`)), timeoutMs);
      this.socket.onopen = () => { clearTimeout(timer); resolve(); };
      const previous = this.socket.onerror;
      this.socket.onerror = event => {
        clearTimeout(timer);
        previous?.call(this.socket, event);
        reject(new Error(`could not reach ${this.address}`));
      };
    });
    await opened;
    this.send({ type: 'hello', protocolVersion: this.protocolVersion, token: this.token, clientId: randomUUID(), resumeFrom: [] });
    await poll('daemon hello', async () => this.hello, () => this.hello !== null, timeoutMs);
  }

  private send(message: unknown) { this.socket.send(JSON.stringify(message)); }

  /** Send one request and settle it from the matching response. The caller
   *  chooses the request id, so a cancel or a repeat can address the same id.
   *  The timeout rejects rather than leaving a promise nobody can settle, so
   *  the runner's final `allSettled` always completes. */
  request(sessionId: string, runtimeId: string, command: unknown, requestId = randomUUID(), timeoutMs = INVOKE_TIMEOUT_MS): Promise<{ status: string; payload?: { type: string; value?: unknown } }> {
    let timer: ReturnType<typeof setTimeout>;
    const outcome = new Promise<{ status: string; payload?: { type: string; value?: unknown } }>((resolve, reject) => {
      timer = setTimeout(() => {
        this.pending.delete(requestId);
        reject(new Error(`the daemon did not answer ${requestId} within ${timeoutMs} ms`));
      }, timeoutMs);
      this.pending.set(requestId, { resolve: resolve as (value: unknown) => void, reject, timer });
    });
    this.send({ type: 'request', requestId, sessionId, runtimeId, command });
    return outcome;
  }

  /** Fire-and-forget: neither the daemon nor the broker replies to these. */
  control(message: unknown) { this.send(message); }

  close() {
    ownedSockets.delete(this);
    try { this.socket.close(); } catch { /* Already closed. */ }
  }
}

const WIRE_OPTIONS = {
  binary: 'opencode',
  cwd: process.cwd(),
  mode: 'default',
  interactionMode: 'default',
  model: null,
  reasoningEffort: null,
  serviceTier: null,
  contextWindow: null,
  agentPreset: null,
  providerCursor: null,
};

async function buildStep(label: string, command: string[]): Promise<void> {
  if (args.has('--skip-build')) return;
  const build = spawn(command[0]!, command.slice(1), { cwd: root, shell: false, stdio: 'inherit', windowsHide: true });
  try { assert.equal(await deadline(exit(build), label, BUILD_LIMIT_MS), 0); }
  finally { if (build.exitCode === null) build.kill(); }
}
/** Resolve a built executable only after its build step has run. */
async function executableFor(candidates: string[], label: string): Promise<string> {
  for (const candidate of candidates) {
    try { await readFile(candidate); return candidate; } catch { /* Try the next spelling. */ }
  }
  throw new Error(`${label}: built executable not found at ${candidates.join(' or ')}`);
}

const sessionId = randomUUID();
const runtimeId = randomUUID();
/** The runtime the broker currently accepts; a replaced runtime changes it. */
let activeRuntimeId = runtimeId;

let client: DaemonSocket | undefined;
let origin = '';
let pages = {} as Partial<Record<PageId, Page>>;
/** The in-flight click that several checks share; it settles on approval. */
let clickInvoke: Promise<InvokeOutcome> | undefined;
/** Native keyboard acquisition before an action, to prove it did not move. */
let focusBeforeClick: number[] = [];
let focusBeforeFill: number[] = [];

try {
  const playwrightPackage: {version: string} = JSON.parse(await readFile(join(root, 'node_modules', 'playwright-core', 'package.json'), 'utf8'));
  assert.equal(typeof playwrightPackage.version, 'string');
  report.versions.playwright = playwrightPackage.version;
  await buildStep('bridge example build', ['cargo', 'build', '--locked', '--package', 'fintwind-core', '--example', 'browser_bridge_e2e', '--target-dir', EXAMPLE_TARGET_DIR]);
  await buildStep('poc host build', ['cargo', 'build', '--locked', '--package', 'fintwind', '--features', 'browser-poc', '--bin', 'fintwind', '--target-dir', POC_TARGET_DIR]);
  const exampleExe = await executableFor([
    join(root, EXAMPLE_TARGET_DIR, 'debug', 'examples', 'browser_bridge_e2e.exe'),
    join(root, EXAMPLE_TARGET_DIR, 'debug', 'browser_bridge_e2e.exe'),
  ], 'bridge example');
  const hostExe = await executableFor([join(root, POC_TARGET_DIR, 'debug', 'fintwind.exe')], 'poc host');
  report.versions.hostSha256 = createHash('sha256').update(await readFile(hostExe)).digest('hex');
  report.versions.exampleSha256 = createHash('sha256').update(await readFile(exampleExe)).digest('hex');
  // From here on the run budget applies: the builds are over.
  budgetStartedAt = Date.now();

  // 1. The daemon under test: the real core server and the real broker.
  example = spawn(exampleExe, [], { cwd: root, shell: false, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
  // The ready line contains an ephemeral bearer token. Parse it in memory;
  // artifacts keep only the non-secret endpoint and protocol metadata.
  let exampleOutput = '';
  example.stdout!.on('data', bytes => { exampleOutput += String(bytes); });
  example.stderr!.on('data', bytes => process.stderr.write(`[example] ${String(bytes)}`));
  if (example.exitCode !== null) throw new Error(`the bridge example exited during startup: ${example.exitCode}`);
  const readyLine = await poll('bridge example ready line', async () => exampleOutput,
    text => text.includes('browser-bridge-e2e') && text.includes('\n'), 60_000);
  try {
    ready = JSON.parse(readyLine.split('\n').find(line => line.includes('browser-bridge-e2e'))!) as NonNullable<typeof ready>;
  } catch { throw new Error('The bridge example did not return a valid ready line.'); }
  const bound = ready;
  assert(bound.address.startsWith('127.0.0.1:'), 'the bridge listener is loopback only');
  assert(Number.isInteger(bound.protocolVersion) && bound.protocolVersion > 0, 'the example prints a protocol version');
  assert(bound.token.length >= 16, 'the example prints a usable bearer token');
  daemonAddress = `ws://${bound.address}/v1`;
  daemonToken = bound.token;
  daemonProtocolVersion = bound.protocolVersion;
  report.versions.protocol = bound.protocolVersion;
  await writeFile(report.artifacts.exampleStdout, JSON.stringify({kind: 'browser-bridge-e2e', address: bound.address,
    protocolVersion: bound.protocolVersion, token: '[redacted]'}) + '\n');
  report.startup.push({ step: 'bridge daemon', status: 'ok', details: daemonAddress });

  // 2. This runner is the agent client: it activates the fake runtime.
  client = openSocket();
  client.protocolVersion = daemonProtocolVersion;
  await client.connect();
  const started = await deadline(client.request(sessionId, runtimeId, { type: 'start', options: WIRE_OPTIONS }), 'runtime start', 20_000);
  assert.equal(started.status, 'ok', `the fake runtime must activate: ${JSON.stringify(started)}`);
  report.startup.push({ step: 'fake runtime', status: 'ok', details: `${sessionId}/${runtimeId}` });

  // 3. The GUI host, with the bridge it must connect to. The bridge address is
  //    a ws:// URL: the client prepends nothing and rejects an http scheme.
  fixture = startFixture(runId);
  origin = fixture.origin;
  const cdpPort = await unusedPort();
  try {
    const probe = await fetch(`http://127.0.0.1:${cdpPort}/json/version`, { signal: AbortSignal.timeout(500) });
    assert(!probe.ok, 'the selected CDP port must not already be in use');
  } catch { /* Expected: no listener. */ }
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.toUpperCase().startsWith('WEBVIEW2_')));
  host = spawn(hostExe, [
    '--browser-poc',
    `--cdp-port=${cdpPort}`,
    `--fixture-origin=${origin}`,
    `--artifact-dir=${output}`,
    `--run-id=${runId}`,
    `--bridge-address=ws://${bound.address}`,
    `--bridge-token=${daemonToken}`,
    `--bridge-session=${sessionId}`,
    `--bridge-runtime=${runtimeId}`,
  ], { cwd: root, env, shell: false, windowsHide: false, stdio: ['ignore', 'pipe', 'pipe'] });
  host.stdout!.pipe(createWriteStream(join(output, 'host.stdout.log')));
  host.stderr!.pipe(createWriteStream(join(output, 'host.stderr.log')));
  host.on('error', error => { report.errors.push(error.message); interrupted = true; });
   const initial = await poll('host ready with a live bridge', state, value => {
     if (!value) return false;
     assert.equal(value.runId, runId);
     assert.equal(value.pid, host?.pid);
    const failed = value.pages.filter(page => page.error);
    assert.equal(failed.length, 0, JSON.stringify(failed));
    return value.pages.length === 3
      && value.pages.every(page => page.ready && page.url?.includes(runId))
      && value.bridge?.configured === true
      && value.bridge?.connected === true
      && value.browserPages.length === 3;
  }, 60_000);
   assert(initial && initial.bridge?.sessionId === sessionId && initial.bridge?.runtimeId === runtimeId,
    'the host shares this runner\'s session and runtime');
  const pageIds = new Map(initial.browserPages.map(page => [page.page, page.pageId]));
  assert.equal(new Set(pageIds.values()).size, 3, 'every page has its own identity');
  report.startup.push({ step: 'gui host', status: 'ok', details: `pid ${host.pid}, 3 pages, bridge connected` });

  // 4. Playwright reads the fixture; it performs none of the actions.
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`, { noDefaults: true, isLocal: true, timeout: 10_000 });
  report.versions.webview2 = browser.version();
  const context = browser.contexts()[0];
  assert(context, 'WebView2 exposes a default context');
  for (const id of PAGE_IDS) {
    const candidates: Page[] = context.pages().filter(page => {
      const url = new URL(page.url());
      return url.origin === origin && url.pathname === `/page/${id}` && url.searchParams.get('run') === runId;
    });
    assert.equal(candidates.length, 1, `Exactly one owned page for ${id}`);
    pages[id] = candidates[0]!;
  }
  // Fixture setup only: values that must never cross the bridge. The fixture
  // ships no password field and sets no cookie, so this adds the ones the
  // snapshot check needs. A textarea's *default* text lives in textContent, so
  // both are set: a reader that only skips `value` would still leak it.
  await pages.alpha!.evaluate(({passwordValue, hiddenValue, textareaValue, cookieValue, longText}) => {
    const password = document.createElement('input');
    password.id = 'e2e-password'; password.type = 'password'; password.value = passwordValue;
    document.body.append(password);
    const hidden = document.createElement('input');
    hidden.id = 'e2e-hidden'; hidden.type = 'hidden'; hidden.value = hiddenValue;
    document.body.append(hidden);
    const textarea = document.createElement('textarea');
    textarea.id = 'e2e-textarea'; textarea.value = textareaValue;
    textarea.textContent = textareaValue;
    document.body.append(textarea);
    const long = document.createElement('p');
    long.id = 'e2e-long'; long.textContent = longText;
    document.body.append(long);
    document.cookie = `e2e-cookie=${cookieValue}; path=/`;
  }, {passwordValue: PASSWORD_VALUE, hiddenValue: HIDDEN_VALUE, textareaValue: TEXTAREA_VALUE, cookieValue: COOKIE_VALUE, longText: LONG_TEXT});

  const scopeFor = (page: PageId, grantId: string): Scope => ({
    sessionId, runtimeId: activeRuntimeId, pageId: pageIds.get(page)!, grantId,
  });
  const invoke = async (scope: Scope, action: BrowserAction, requestId = randomUUID()): Promise<InvokeOutcome> =>
    track(`invoke ${action.kind}`, (async () => {
      const outcome = await deadline(client!.request(sessionId, activeRuntimeId, { type: 'browserInvoke', scope, action }, requestId), `invoke ${action.kind}`, INVOKE_TIMEOUT_MS);
      // Either an RPC error or a serialized BrowserResult is an answer; neither
      // may be a success for a bad scope.
      if (outcome.status === 'error') return { kind: 'error', message: JSON.stringify(outcome) } as InvokeOutcome;
      const value = outcome.payload?.value as InvokeOutcome | undefined;
      assert(value && typeof value.kind === 'string', `a serialized browser result: ${JSON.stringify(outcome)}`);
      return value;
    })());
  const listShares = async (): Promise<Array<{ scope: Scope; url: string; title: string }>> => {
    const outcome = await deadline(client!.request(sessionId, activeRuntimeId, { type: 'browserList' }), 'browser list', INVOKE_TIMEOUT_MS);
    assert.equal(outcome.status, 'ok', `the bridge answered the list: ${JSON.stringify(outcome)}`);
    return outcome.payload?.value as Array<{ scope: Scope; url: string; title: string }>;
  };
  const fixtureCount = (id: PageId) => pages[id]!.locator('#count-value').textContent();
  /** A request is delivered over a 250 ms host poll, so wait for it. */
  const pendingFor = async (page: PageId, action: string) => {
    const pending = await poll(`${action} pending on ${page}`, requireState, value =>
      value.pendingBrowserRequests.some(request => request.page === page && request.action === action));
    return pending.pendingBrowserRequests.find(request => request.page === page && request.action === action)!;
  };
  const focusGains = async () => (await requireState()).pages.map(page => page.nativeFocusGains);
  const expectNoFocusChange = async (before: number[], what: string) => {
    const after = await focusGains();
    assert.deepEqual(after, before, `${what} moved the native keyboard away from its owner`);
  };

  await check('an unshared page refuses an action and the fixture is untouched', async () => {
    const scope = scopeFor('alpha', randomUUID());
    const result = await invoke(scope, { kind: 'click', selector: '#count' });
    assert.equal(result.kind, 'error', JSON.stringify(result));
    assert.equal(await fixtureCount('alpha'), '0');
    assert.deepEqual(await listShares(), [], 'nothing is listed for a page nobody shared');
    return 'A click on an unshared page is refused before the GUI hears about it.';
  });

  await check('sharing one page maps exactly through the daemon', async () => {
    const grantId = randomUUID();
    const focusBeforeShare = await focusGains();
    await control('share-page', { pageId: 'alpha', grantId, runtimeId: activeRuntimeId });
    await poll('alpha shared', requireState, value => value.browserShares.length === 1
      && value.browserShares[0]?.page === 'alpha'
      && value.browserShares[0]?.scope.grantId === grantId
      && value.browserShares[0]?.scope.sessionId === sessionId
      && value.browserShares[0]?.scope.runtimeId === activeRuntimeId
      && value.browserShares[0]?.scope.pageId === pageIds.get('alpha'));
    const listed = await listShares();
    assert.equal(listed.length, 1, JSON.stringify(listed));
    assert.equal(listed[0]!.scope.grantId, grantId);
    assert.equal(listed[0]!.scope.pageId, pageIds.get('alpha'));
    assert.equal(listed[0]!.scope.sessionId, sessionId);
    assert.equal(listed[0]!.url, `${origin}/page/alpha?run=${runId}`);
    // The collaboration bar takes layout space; the fixture's own controls must
    // stay visible and inside the viewport, or later clicks target nothing.
    const bounds = await pages.alpha!.evaluate(() => {
      const read = (selector: string) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const rect = element.getBoundingClientRect();
        return { width: rect.width, height: rect.height, inViewport: rect.x >= 0 && rect.y >= 0 && rect.right <= innerWidth && rect.bottom <= innerHeight };
      };
      return { count: read('#count'), name: read('#name') };
    });
    assert(bounds.count && bounds.count.width > 0 && bounds.count.inViewport, JSON.stringify(bounds));
    assert(bounds.name && bounds.name.width > 0 && bounds.name.inViewport, JSON.stringify(bounds));
    // Sharing is an explicit human action: it keeps native typing where it is
    // instead of stealing the keyboard for the agent.
    await expectNoFocusChange(focusBeforeShare, 'sharing a page');
    return 'One lease, one page, exact scope; the bar shrank the viewport and the controls stayed visible.';
  });

  await check('a snapshot carries no input values, cookies or unbounded page', async () => {
    const share = (await requireState()).browserShares[0]!;
    const snapshot = await invoke(share.scope, { kind: 'snapshot' });
    assert.equal(snapshot.kind, 'ok', JSON.stringify(snapshot));
    const value = snapshot.kind === 'ok' ? snapshot.value as { url: string; title: string; text: string; controls: unknown[]; truncated: boolean } : null!;
    assert.equal(value.url, `${origin}/page/alpha?run=${runId}`);
    assert(JSON.stringify(value).includes('count'), 'the control name is visible');
    assert(!JSON.stringify(value).includes(PASSWORD_VALUE), 'a password value must never cross the bridge');
    assert(!JSON.stringify(value).includes(HIDDEN_VALUE), 'a hidden input value must never cross the bridge');
    assert(!JSON.stringify(value).includes(TEXTAREA_VALUE), 'a textarea default value must never cross the bridge');
    assert(!JSON.stringify(value).includes(COOKIE_VALUE), 'a cookie value must never cross the bridge');
    assert(value.text.length <= 8000 && !value.text.includes(LONG_TEXT), 'the long page text is bounded, not dumped');
    const bytes = new TextEncoder().encode(JSON.stringify(value)).length;
    assert(bytes <= 28000, `a snapshot is byte-bounded, got ${bytes}`);
    assert(value.controls.length > 0, 'controls are described');
    return `Snapshot ${bytes} bytes, ${value.controls.length} controls, no secret values.`;
  });

  await check('a click waits for approval and leaves the page alone', async () => {
    const share = (await requireState()).browserShares[0]!;
    focusBeforeClick = await focusGains();
    // Start the call and keep the promise: the answer arrives only on approval.
    clickInvoke = invoke(share.scope, { kind: 'click', selector: '#count' });
    const pending = await pendingFor('alpha', 'click');
    assert.equal(await fixtureCount('alpha'), '0', 'no mutation before approval');
    const trusted = await pages.alpha!.evaluate(() => (window as unknown as { pocEvents: { clickTrusted: boolean } }).pocEvents);
    assert.equal(trusted.clickTrusted, false, 'no click happened yet');
    assert.deepEqual(await focusGains(), focusBeforeClick, 'a pending request moved the native keyboard');
    return `Pending request ${pending.requestId} for ${pending.detail}; the page is untouched.`;
  });

  await check('approval runs the pending click through the shared page', async () => {
    const pending = await pendingFor('alpha', 'click');
    await control('approve-browser', { browserRequestId: pending.requestId });
    const outcome = await clickInvoke!;
    assert.equal(outcome.kind, 'ok', JSON.stringify(outcome));
    clickInvoke = undefined;
    await expectNoFocusChange(focusBeforeClick, 'the approved click');
    return 'The approval went through the same view API the approval bar uses.';
  });

  await check('the approved click is trusted input on the shared page only', async () => {
    await poll('alpha counted once', () => fixtureCount('alpha'), value => value === '1');
    const trusted = await pages.alpha!.evaluate(() => (window as unknown as { pocEvents: { clickTrusted: boolean } }).pocEvents);
    assert.equal(trusted.clickTrusted, true, 'the click must be trusted native input');
    assert.equal(await fixtureCount('beta'), '0');
    assert.equal(await fixtureCount('gamma'), '0');
    assert.equal((await requireState()).pendingBrowserRequests.length, 0, 'the request settled');
    return 'One trusted click on alpha; beta and gamma untouched.';
  });

  await check('an approved fill types the trusted value', async () => {
    const share = (await requireState()).browserShares[0]!;
    focusBeforeFill = await focusGains();
    // Do not await yet: an unapproved fill is pending, not failed.
    const filled = invoke(share.scope, { kind: 'fill', selector: '#name', text: 'Collaboration fill' });
    const pending = await pendingFor('alpha', 'fill');
    assert.equal(await pages.alpha!.locator('#name').inputValue(), '', 'no text is typed before approval');
    await control('approve-browser', { browserRequestId: pending.requestId });
    const outcome = await filled;
    assert.equal(outcome.kind, 'ok', JSON.stringify(outcome));
    await poll('alpha filled', () => pages.alpha!.locator('#name-value').textContent(), value => value === 'Collaboration fill');
    const trusted = await pages.alpha!.evaluate(() => (window as unknown as { pocEvents: { inputTrusted: boolean } }).pocEvents);
    assert.equal(trusted.inputTrusted, true, 'the fill must be trusted native input');
    assert.equal(await pages.alpha!.locator('#name').inputValue(), 'Collaboration fill', 'only the fill\'s own text landed');
    await expectNoFocusChange(focusBeforeFill, 'the approved fill');
    return 'The fill was approved, typed by the native path, and only its own text landed.';
  });

  await check('a rejected click mutates nothing', async () => {
    const share = (await requireState()).browserShares[0]!;
    const rejected = invoke(share.scope, { kind: 'click', selector: '#count' });
    const pending = await pendingFor('alpha', 'click');
    await control('reject-browser', { browserRequestId: pending.requestId });
    const outcome = await rejected;
    assert.equal(outcome.kind, 'error', JSON.stringify(outcome));
    assert.equal(await fixtureCount('alpha'), '1', 'the rejected click did not run');
    assert.equal((await requireState()).pendingBrowserRequests.length, 0);
    return 'The rejection reached the caller as an error and the page kept its count.';
  });

  await check('a foreign session, runtime, grant or page is refused', async () => {
    const share = (await requireState()).browserShares[0]!;
    const foreign = [
      { ...share.scope, sessionId: randomUUID() },
      { ...share.scope, runtimeId: randomUUID() },
      { ...share.scope, grantId: randomUUID() },
      { ...share.scope, pageId: randomUUID() },
    ];
    for (const scope of foreign) {
      const result = await invoke(scope, { kind: 'snapshot' });
      assert.equal(result.kind, 'error', `${JSON.stringify(scope)} must be refused: ${JSON.stringify(result)}`);
    }
    assert.equal(await fixtureCount('alpha'), '1');
    return 'Four wrong scopes, four refusals, no page access.';
  });

  await check('revoking while a request is pending fails the call and the page', async () => {
    const share = (await requireState()).browserShares[0]!;
    const grant = share.scope.grantId;
    const focusBefore = await focusGains();
    const pending = invoke(share.scope, { kind: 'click', selector: '#count' });
    await pendingFor('alpha', 'click');
    await control('revoke-page', { pageId: 'alpha', runtimeId: activeRuntimeId });
    const outcome = await pending;
    assert.equal(outcome.kind, 'error', JSON.stringify(outcome));
    assert.match(outcome.kind === 'error' ? outcome.message : '', /revok|no longer|share|expired/i);
    assert.equal(await fixtureCount('alpha'), '1', 'a revoked grant cannot mutate');
    const after = await invoke(share.scope, { kind: 'snapshot' });
    assert.equal(after.kind, 'error', JSON.stringify(after));
    assert.equal((await requireState()).browserShares.length, 0, 'nothing stays shared');
    await expectNoFocusChange(focusBefore, 'the revoked request');
    return `The in-flight call failed, grant ${grant} is dead, and a new call is refused.`;
  });

  await check('a new grant replaces the old one', async () => {
    const oldGrant = randomUUID();
    await control('share-page', { pageId: 'alpha', grantId: oldGrant, runtimeId: activeRuntimeId });
    await poll('alpha shared again', requireState, value => value.browserShares.length === 1);
    const superseded = await invoke(scopeFor('alpha', oldGrant), { kind: 'snapshot' });
    assert.equal(superseded.kind, 'ok', JSON.stringify(superseded));
    const newGrant = randomUUID();
    await control('share-page', { pageId: 'alpha', grantId: newGrant, runtimeId: activeRuntimeId });
    await poll('alpha reshared with a new grant', requireState, value => value.browserShares.length === 1 && value.browserShares[0]?.scope.grantId === newGrant);
    const stale = await invoke(scopeFor('alpha', oldGrant), { kind: 'snapshot' });
    assert.equal(stale.kind, 'error', `the superseded grant must not work: ${JSON.stringify(stale)}`);
    const live = await invoke(scopeFor('alpha', newGrant), { kind: 'snapshot' });
    assert.equal(live.kind, 'ok', JSON.stringify(live));
    return 'The new lease works; the previous one is dead even though the page never changed.';
  });

  await check('navigation invalidates the grant', async () => {
    const share = (await requireState()).browserShares[0]!;
    const target = `${origin}/page/alpha?run=${runId}#navigated`;
    const focusBefore = await focusGains();
    const navigation = invoke(share.scope, { kind: 'navigate', url: target });
    await pendingFor('alpha', 'navigate');
    const pending = await pendingFor('alpha', 'navigate');
    await control('approve-browser', { browserRequestId: pending.requestId });
    // A navigation acknowledges dispatch, not success: either outcome is
    // acceptable, and what must hold is the URL change plus the dead lease.
    const outcome = await navigation;
    if (outcome.kind !== 'ok') assert.match(outcome.message, /issue|uncertain|navigat|timeout/i, JSON.stringify(outcome));
    await poll('alpha navigated', async () => pages.alpha!.url(), url => url.includes('#navigated'), 15_000);
    const stale = await invoke(share.scope, { kind: 'snapshot' });
    assert.equal(stale.kind, 'error', `the pre-navigation grant must not work: ${JSON.stringify(stale)}`);
    await expectNoFocusChange(focusBefore, 'the approved navigation');
    // The moved document needs its own explicit share.
    const fresh = randomUUID();
    await control('share-page', { pageId: 'alpha', grantId: fresh, runtimeId: activeRuntimeId });
    await poll('alpha reshared after navigation', requireState, value => value.browserShares.length === 1);
    const live = await invoke(scopeFor('alpha', fresh), { kind: 'snapshot' });
    assert.equal(live.kind, 'ok', JSON.stringify(live));
    return `Navigated to ${target}; the pre-navigation grant is dead and a fresh share works.`;
  });

  await check('cancelling by the caller\'s RPC id stops the GUI and changes nothing', async () => {
    const grant = randomUUID();
    await control('share-page', { pageId: 'alpha', grantId: grant, runtimeId: activeRuntimeId });
    await poll('alpha shared for the caller cancel', requireState, value => value.browserShares.length === 1 && value.browserShares[0]?.scope.grantId === grant);
    const rpcId = randomUUID();
    const cancelled = invoke(scopeFor('alpha', grant), { kind: 'click', selector: '#count' }, rpcId);
    const pending = await pendingFor('alpha', 'click');
    // The caller addresses its own RPC id, so the GUI must also be told.
    client!.control({ type: 'browserCancel', requestId: rpcId });
    const outcome = await cancelled;
    assert.equal(outcome.kind, 'error', JSON.stringify(outcome));
    assert.match(outcome.kind === 'error' ? outcome.message : '', /cancel/i);
    assert.equal(await fixtureCount('alpha'), '1', 'a cancelled click did not run');
    await poll('the GUI abandoned its cancelled approval', requireState, value => value.pendingBrowserRequests.length === 0);
    return `RPC ${rpcId} cancelled by its caller; request ${pending.requestId} never ran.`;
  });

  await check('a cancel that arrives before the invoke refuses it', async () => {
    const grant = randomUUID();
    await control('share-page', { pageId: 'alpha', grantId: grant, runtimeId: activeRuntimeId });
    await poll('alpha shared for the tombstone', requireState, value => value.browserShares.length === 1 && value.browserShares[0]?.scope.grantId === grant);
    const rpcId = randomUUID();
    // The cancel is recorded before the invoke is even dispatched, so a tomb-
    // stone must stop the action from ever being offered to the GUI.
    client!.control({ type: 'browserCancel', requestId: rpcId });
    await Bun.sleep(300);
    const outcome = await invoke(scopeFor('alpha', grant), { kind: 'click', selector: '#count' }, rpcId);
    assert.equal(outcome.kind, 'error', JSON.stringify(outcome));
    assert.match(outcome.kind === 'error' ? outcome.message : '', /cancel/i);
    assert.equal((await requireState()).pendingBrowserRequests.length, 0, 'no approval may appear');
    assert.equal(await fixtureCount('alpha'), '1', 'a cancelled-before-start request never runs');
    return `RPC ${rpcId} was cancelled before dispatch and stayed cancelled.`;
  });

  await check('a repeated request id is refused, not answered twice', async () => {
    const grant = randomUUID();
    await control('share-page', { pageId: 'alpha', grantId: grant, runtimeId: activeRuntimeId });
    await poll('alpha shared for the repeat', requireState, value => value.browserShares.length === 1 && value.browserShares[0]?.scope.grantId === grant);
    const rpcId = randomUUID();
    const first = await invoke(scopeFor('alpha', grant), { kind: 'snapshot' }, rpcId);
    assert.equal(first.kind, 'ok', JSON.stringify(first));
    const countBefore = await fixtureCount('alpha');
    // The same id again: the broker refuses it instead of running it twice, so
    // no second approval may appear for it.
    const repeat = await invoke(scopeFor('alpha', grant), { kind: 'click', selector: '#count' }, rpcId);
    assert.equal(repeat.kind, 'error', `a repeated id must be refused: ${JSON.stringify(repeat)}`);
    assert.match(repeat.kind === 'error' ? repeat.message : '', /already attempted|retry/i);
    assert.equal((await requireState()).pendingBrowserRequests.length, 0, 'no new approval for a spent id');
    assert.equal(await fixtureCount('alpha'), countBefore, 'a repeated id does not act twice');
    return `RPC ${rpcId} ran once; its repeat was refused and produced no second action.`;
  });

  await check('closing the shared page fails later requests', async () => {
    const grant = randomUUID();
    await control('share-page', { pageId: 'alpha', grantId: grant, runtimeId: activeRuntimeId });
    await poll('alpha shared before close', requireState, value => value.browserShares.length === 1);
    await control('close-page', { pageId: 'alpha' });
    await poll('alpha closed', requireState, value => value.pages.every(page => page.id !== 'alpha'));
    const after = await invoke(scopeFor('alpha', grant), { kind: 'snapshot' });
    assert.equal(after.kind, 'error', JSON.stringify(after));
    assert.equal((await requireState()).browserShares.length, 0, 'the closed page is not shared anymore');
    assert.equal(context.pages().length, 2, 'no orphan target');
    return 'The closed page\'s grant is gone and a later call is refused.';
  });

  await check('another client\'s answer is refused', async () => {
    const grant = randomUUID();
    await control('share-page', { pageId: 'beta', grantId: grant, runtimeId: activeRuntimeId });
    await poll('beta shared', requireState, value => value.browserShares.some(share => share.page === 'beta'));
    const spoof = openSocket();
    spoof.protocolVersion = daemonProtocolVersion;
    await spoof.connect();
    const answered = invoke(scopeFor('beta', grant), { kind: 'click', selector: '#count' });
    const pending = await pendingFor('beta', 'click');
    // A second connection answers for a page it does not own.
    spoof.control({ type: 'browserResult', requestId: pending.requestId, result: { kind: 'ok', value: { issued: true } } });
    await Bun.sleep(600);
    assert.equal(await fixtureCount('beta'), '0', 'a forged answer must not execute');
    assert.equal((await requireState()).pendingBrowserRequests[0]?.requestId, pending.requestId, 'the real request is still pending');
    await control('approve-browser', { browserRequestId: pending.requestId });
    const outcome = await answered;
    assert.equal(outcome.kind, 'ok', JSON.stringify(outcome));
    await poll('beta counted once', () => fixtureCount('beta'), value => value === '1');
    assert.equal(await fixtureCount('gamma'), '0');
    spoof.close();
    return 'The forged result was ignored; the owner\'s approval ran the action exactly once.';
  });

  await check('an over-large publish is refused and shows no share', async () => {
    // A connection that publishes more pages than the broker allows is refused
    // whole. The host never sent those pages, so this asserts the daemon's view
    // and the host's own view both stayed exactly as they were.
    const spam = openSocket();
    spam.protocolVersion = daemonProtocolVersion;
    await spam.connect();
    const before = await listShares();
    const attempted = Array.from({ length: 20 }, () => ({
      scope: { sessionId, runtimeId: activeRuntimeId, pageId: randomUUID(), grantId: randomUUID() },
      url: `${origin}/page/alpha?run=${runId}`,
      title: 'spam',
    }));
    spam.control({ type: 'browserPublish', pages: attempted });
    const refusal = await poll('the over-large publish is refused', async () => spam.notifications, list => list.some(entry => entry.type === 'browserShareRejected'), 10_000);
    const rejected = refusal.find(entry => entry.type === 'browserShareRejected')!;
    assert(Array.isArray(rejected.scopes) && rejected.scopes.length >= 1, `the refusal names the attempted scopes: ${JSON.stringify(rejected)}`);
    if (JSON.stringify(rejected).includes(daemonToken)) throw new Error('the refusal leaked the bridge token');
    assert.deepEqual(await listShares(), before, 'a refused publish adds no share');
    const hostView = await requireState();
    assert.equal(hostView.browserShares.length, before.length, 'the host does not show pages it could not publish');
    for (const page of attempted.slice(0, 3)) {
      const result = await invoke(page.scope, { kind: 'snapshot' });
      assert.equal(result.kind, 'error', `a page the daemon refused must not answer: ${JSON.stringify(result)}`);
    }
    spam.close();
    return `The daemon refused the 20-page publish whole; the live share count stayed ${before.length}.`;
  });

  await check('a cancelled request cannot be approved afterwards', async () => {
    const grant = randomUUID();
    await control('share-page', { pageId: 'gamma', grantId: grant, runtimeId: activeRuntimeId });
    await poll('gamma shared for the owner cancel', requireState, value => value.browserShares.some(share => share.page === 'gamma'));
    const cancelled = invoke(scopeFor('gamma', grant), { kind: 'click', selector: '#count' });
    const pending = await pendingFor('gamma', 'click');
    // The owner cancels with the internal id the daemon gave it.
    await control('cancel-browser', { browserRequestId: pending.requestId });
    const outcome = await cancelled;
    assert.equal(outcome.kind, 'error', JSON.stringify(outcome));
    // A late approval for the same request must not resurrect it.
    await control('approve-browser', { browserRequestId: pending.requestId });
    await Bun.sleep(700);
    assert.equal(await fixtureCount('gamma'), '0', 'a cancelled request must not run even if approved afterwards');
    assert.equal((await requireState()).pendingBrowserRequests.length, 0, 'no approval is left waiting');
    return `Request ${pending.requestId} stayed cancelled; a later approval had no effect.`;
  });

  await check('an owner disconnect drops every lease and a reconnect starts empty', async () => {
    // The host drops its own daemon connection, the way closing the app would.
    await control('share-page', { pageId: 'gamma', grantId: randomUUID(), runtimeId: activeRuntimeId });
    await poll('gamma shared before the disconnect', requireState, value => value.browserShares.some(share => share.page === 'gamma'));
    await control('bridge-disconnect', {});
    await poll('the host dropped its connection', requireState, value => value.bridge?.connected === false, 20_000);
    await poll('the host revoked its own shares', requireState, value => value.browserShares.length === 0, 20_000);
    const disconnectedShares = await poll('the daemon revoked the disconnected owner leases', listShares, shares => shares.length === 0);
    assert.deepEqual(disconnectedShares, [], 'a disconnect must not leave a lease behind');
    await control('bridge-connect', {});
    await poll('the host reconnected', requireState, value => value.bridge?.connected === true, 20_000);
    const reconnectedShares = await poll('the reconnected owner has no inherited lease', listShares, shares => shares.length === 0);
    assert.deepEqual(reconnectedShares, [], 'a reconnect must not inherit a lease');
    const fresh = randomUUID();
    await control('share-page', { pageId: 'gamma', grantId: fresh, runtimeId: activeRuntimeId });
    await poll('gamma reshared on the new connection', requireState, value => value.browserShares.some(share => share.page === 'gamma' && share.scope.grantId === fresh), 20_000);
    const live = await invoke(scopeFor('gamma', fresh), { kind: 'snapshot' });
    assert.equal(live.kind, 'ok', JSON.stringify(live));
    return 'No lease survived the disconnect, and the reconnected host had to be granted one.';
  });

  await check('a replaced runtime clears the GUI share', async () => {
    const share = (await requireState()).browserShares.find(entry => entry.page === 'gamma');
    assert(share, 'gamma is shared before the runtime is replaced');
    const replaced = randomUUID();
    const started = await deadline(client!.request(sessionId, replaced, { type: 'start', options: WIRE_OPTIONS }), 'runtime replacement', 20_000);
    assert.equal(started.status, 'ok', `the replacement runtime must activate: ${JSON.stringify(started)}`);
    await poll('the replaced runtime revoked the share', requireState, value => value.browserShares.length === 0);
    assert.deepEqual(await listShares(), [], 'the daemon holds no page for the old runtime');
    const stale = await invoke(share.scope, { kind: 'snapshot' });
    assert.equal(stale.kind, 'error', `the old runtime's grant must not work: ${JSON.stringify(stale)}`);
    // From here on the broker answers this session's new runtime only, so a
    // new share binds the new runtime.
    activeRuntimeId = replaced;
    const fresh = randomUUID();
    await control('share-page', { pageId: 'gamma', grantId: fresh, runtimeId: activeRuntimeId });
    await poll('gamma shared under the new runtime', requireState, value => value.browserShares.some(entry => entry.page === 'gamma' && entry.scope.runtimeId === replaced));
    const live = await invoke(scopeFor('gamma', fresh), { kind: 'snapshot' });
    assert.equal(live.kind, 'ok', JSON.stringify(live));
    return 'The replaced runtime revoked the old grant, and a fresh share works under the new runtime.';
  });
} catch (error) {
  // Never surface the ready line here: it carries the bearer token.
  report.errors.push(error instanceof Error ? error.stack ?? error.message : String(error));
  console.error('[X] E2E could not complete:', error);
} finally {
  // Every promise this runner started settles here, so a check that failed
  // while another was still awaiting cannot end the run with an unhandled
  // rejection and no report.
  await Promise.allSettled(inflight.map(entry => entry.promise));
  if (browser) {
    try { await deadline(browser.close(), 'Disconnect cleanup', 5_000); }
    catch (error) { record('cleanup', 'playwright disconnect', error); }
  }
  for (const socket of [...ownedSockets]) {
    try { socket.close(); } catch { /* Already closed. */ }
  }
  if (host && host.exitCode === null && host.signalCode === null) {
    try {
      const exited = exit(host);
      await control('shutdown', {});
      await deadline(exited, 'Host shutdown', 5_000);
      report.cleanup.push({ step: 'host shutdown', status: 'ok', details: `pid ${host.pid}` });
    } catch (error) {
      // Only the pid this runner created is ever killed; never other processes.
      if (host.pid && host.exitCode === null && host.signalCode === null) {
        const kill = spawn('taskkill', ['/PID', String(host.pid), '/T', '/F'], { shell: false, windowsHide: true, stdio: 'ignore' });
        try {
          assert.equal(await deadline(exit(kill), 'Host process cleanup', 5_000), 0);
          report.cleanup.push({ step: 'host taskkill', status: 'ok', details: `pid ${host.pid}` });
        } catch (killError) { record('cleanup', 'host taskkill', killError); }
      } else {
        record('cleanup', 'host shutdown', error);
      }
    }
  }
  if (example && example.exitCode === null && example.signalCode === null && daemonAddress) {
    try {
      // Connect first: a control on an unconnected socket is a no-op.
      const shutdown = openSocket();
      shutdown.protocolVersion = daemonProtocolVersion;
      await shutdown.connect();
      shutdown.control({ type: 'shutdown' });
      await deadline(exit(example), 'Example shutdown', 8_000);
      report.cleanup.push({ step: 'example shutdown', status: 'ok', details: 'ClientMessage::Shutdown' });
      shutdown.close();
    } catch (error) {
      // The exact pid was spawned by this runner; never kill other processes.
      if (example.pid && example.exitCode === null && example.signalCode === null) {
        const kill = spawn('taskkill', ['/PID', String(example.pid), '/T', '/F'], { shell: false, windowsHide: true, stdio: 'ignore' });
        try {
          assert.equal(await deadline(exit(kill), 'Example process cleanup', 5_000), 0);
          report.cleanup.push({ step: 'example taskkill', status: 'ok', details: `pid ${example.pid}` });
        } catch (killError) { record('cleanup', 'example taskkill', killError); }
      } else {
        record('cleanup', 'example shutdown', error);
      }
    }
  }
  if (fixture) {
    try { fixture.stop(); report.cleanup.push({ step: 'fixture stop', status: 'ok', details: 'both loopback servers stopped' }); }
    catch (error) { record('cleanup', 'fixture stop', error); }
  }
  if (interrupted && !report.errors.includes('E2E interrupted.')) report.errors.push('E2E interrupted.');
  report.finishedAt = new Date().toISOString();
  if (report.checks.length !== 21) report.errors.push(`Expected 21 behavior checks; ran ${report.checks.length}.`);
  report.status = report.errors.length || report.checks.some(check => check.status === 'failed')
    || report.cleanup.some(step => step.status === 'failed') ? 'failed' : 'passed';
  await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2));
  console.log(`Report: ${join(output, 'report.json')}`);
  process.exitCode = report.status === 'passed' ? 0 : 1;
}
