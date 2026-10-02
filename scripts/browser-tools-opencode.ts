/**
 * Phase-three behavioral E2E for the Fintwind OpenCode browser tools.
 *
 * This runner drives the *real* chain end to end and only fakes the two things
 * outside Fintwind's control, in clearly separated fixtures:
 *
 *   daemon Start/Prompt  ->  real OpenCode driver  ->  real `opencode serve`
 *   process  ->  real browser plugin tool.execute (real sessionID + signal)
 *   ->  private `/v1/browser-tools` socket  ->  real browser broker
 *   ->  GUI protocol owner  ->  results  ->  model provider
 *
 * The GUI is a **protocol owner** (`simulated GUI`), not the native WebView2
 * host; the model provider is a **fake OpenAI-compatible stub** that emits
 * deterministic tool calls and records exactly what the model receives. The
 * report labels every check with which of these is real, and the runner never
 * claims a real provider or a full native GUI.
 *
 * Failure modes this runner is built to surface (written before the checks):
 * - the plugin does not load but ordinary `Start` still succeeds in degraded
 *   mode: only a successful tool round-trip proves browser activation;
 * - the sixteen tools are not registered, or one names a page the model did not
 *   actually see (broken list -> snapshot argument wiring);
 * - OpenCode's built-in desktop browser tools still reach the model, so it
 *   blames a disconnected host and never uses the in-app browser;
 * - the model context never receives the Fintwind browser instruction, so it
 *   treats an empty share list as a connection failure;
 * - a model receives a snapshot that is not tagged as an untrusted source;
 * - a mutation reaches the GUI before a human approves it, or a rejected
 *   mutation acts anyway;
 * - an opaque element reference is ignored and a CSS selector is guessed;
 * - `fintwind_browser_open` is answered by a fixture instead of really
 *   reaching the daemon, where no Fintwind window hosts the session;
 * - two native sessions on one server cross scope, or an unmapped session
 *   falls back to someone else's page;
 * - an interrupt (through OpenCode `/api/session/{id}/interrupt`) leaves the
 *   browser work pending, or a disconnect causes a re-sent action;
 * - the fake provider is called a non-deterministic number of times.
 *
 * Only the two files this runner owns plus the example it builds are touched:
 * it never edits production Rust, resources, the protocol, package/docs, or the
 * existing `browser-collaboration` script the main agent is changing.
 */
import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { BUILTIN_BROWSER_TOOL_IDS, FINTWIND_BROWSER_TOOL_NAMES } from '../resources/opencode-browser-plugin.ts';
import { startFakeProvider, type PlanStep } from './fixtures/browser-tools/fake-provider.ts';
import { GuiProtocolOwner, PNG_1X1_TRANSPARENT_BASE64 } from './fixtures/browser-tools/gui-protocol-owner.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
// Everything here is loopback; inherited proxies must not touch it.
const noProxy = [process.env.NO_PROXY, process.env.no_proxy, '127.0.0.1', 'localhost', '::1']
  .filter(Boolean)
  .join(',');
process.env.NO_PROXY = noProxy;
process.env.no_proxy = noProxy;

const EXAMPLE_TARGET_DIR = 'target/browser-bridge-e2e';
const BUILD_LIMIT_MS = 900_000;
/** Measured from the moment the builds finish, so a slow compiler cannot turn a
 *  healthy run into a false failure. */
const RUN_LIMIT_MS = 300_000;
/** A held approval must not hang the run; the turn it blocks gets its own gate. */
const INVOKE_TIMEOUT_MS = 25_000;
/**
 * Every check this runner performs, in the order it performs them. A run that
 * executes fewer checks failed early and must not be reported as a pass, so the
 * count is compared against the executed checks before the status is written.
 */
const EXPECTED_CHECKS = 25;

const args = new Set(process.argv.slice(2));
for (const arg of args) {
  if (arg !== '--skip-build') throw new Error(`Unknown argument: ${arg}`);
}
assert(process.platform === 'win32', 'This E2E requires Windows.');

const opencodeBinary = process.env.FINTWIND_OPENCODE_BINARY ?? 'E:\\bun\\bin\\opencode.exe';

const runId = randomUUID();
const output = join(root, 'target', 'browser-tools-opencode', 'runs', runId);
const stateDir = join(output, 'state');
const opencodeConfigDir = join(output, 'oc-config');
const xdgConfig = join(output, 'xdg', 'config');
const xdgData = join(output, 'xdg', 'data');
const xdgCache = join(output, 'xdg', 'cache');
const workspaceDir = join(output, 'workspace');
await mkdir(stateDir, { recursive: true });
await mkdir(opencodeConfigDir, { recursive: true });
await mkdir(xdgConfig, { recursive: true });
await mkdir(xdgData, { recursive: true });
await mkdir(xdgCache, { recursive: true });
await mkdir(workspaceDir, { recursive: true });

const report = {
  runId,
  status: 'running' as 'running' | 'passed' | 'failed',
  startedAt: new Date().toISOString(),
  finishedAt: '',
  versions: {
    opencode: '',
    opencodePath: opencodeBinary,
    daemon: '',
    protocol: 0,
    exampleSha256: '',
    bun: Bun.version,
    realness: {
      opencode: 'real',
      daemonAndDriver: 'real',
      brokerAndRegistry: 'real',
      modelProvider: 'fake-loopback-openai-compatible',
      gui: 'simulated-protocol-owner',
    },
  },
  fixture: {
    fakeProviderCalls: 0,
    turns: [] as Array<{ marker: string; toolCalls: number; turnCalls: number; results: unknown[] }>,
    /** The distinct tool names OpenCode declared to the provider. */
    providerToolNames: [] as string[],
    /** Built-in desktop browser tool ids that reached the model (must be none). */
    builtinBrowserToolsSeen: [] as string[],
    /** Provider requests that carried the Fintwind browser instruction. */
    fintwindBrowserInstructionRequests: 0,
    /** Tool-less auxiliary requests (the documented title path), counted, not failed. */
    auxiliaryProviderRequests: 0,
    expectedChecks: EXPECTED_CHECKS,
  },
  /** High-level, language about what is real vs. blocked and why. */
  findings: [] as string[],
  checks: [] as Array<{ name: string; status: 'passed' | 'failed' | 'skipped'; details: string; durationMs: number; realness: string }>,
  errors: [] as string[],
  startup: [] as Array<{ step: string; status: 'ok' | 'failed'; details: string }>,
  cleanup: [] as Array<{ step: string; status: 'ok' | 'failed'; details: string }>,
  artifacts: { report: join(output, 'report.json') },
};
await writeFile(report.artifacts.report, JSON.stringify(report, null, 2));

let daemon: ChildProcess | undefined;
let exampleStdout = '';
let budgetStartedAt = 0;
let interrupted = false;
const interrupt = () => { interrupted = true; };
process.on('SIGINT', interrupt);
process.on('SIGTERM', interrupt);

const ownedSockets = new Set<DaemonSocket>();
const inflight: Array<{ label: string; promise: Promise<unknown> }> = [];
function track<T>(label: string, promise: Promise<T>): Promise<T> {
  const guarded = promise.then(v => v, e => { throw e; });
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
async function poll<T>(label: string, read: () => Promise<T> | T, accept: (value: T) => boolean, ms = 20_000): Promise<T> {
  const budget = Math.min(ms, await remainingMs());
  const until = Date.now() + budget;
  while (Date.now() < until) {
    if (interrupted) throw new Error('E2E interrupted.');
    let value: T;
    try { value = await deadline(Promise.resolve(read()), label, 3_000); } catch { await Bun.sleep(80); continue; }
    if (accept(value)) return value;
    await Bun.sleep(80);
  }
  throw new Error(`${label} timed out`);
}
async function check(name: string, realness: string, run: () => Promise<string | void>) {
  const start = performance.now();
  try {
    await remainingMs();
    const details = await deadline(run(), name, Math.max(8_000, await remainingMs()));
    report.checks.push({ name, status: 'passed', details: details ?? 'Verified.', durationMs: Math.round(performance.now() - start), realness });
    console.log(`[OK] ${name}`);
  } catch (error) {
    const details = error instanceof Error ? error.message : String(error);
    report.checks.push({ name, status: 'failed', details, durationMs: Math.round(performance.now() - start), realness });
    console.error(`[X] ${name}: ${details}`);
    try {
      await writeFile(join(output, `failure-${report.checks.length}.json`), JSON.stringify({
        provider: { turnCalls: fake.turnCalls, toolResults: fake.toolResults, requests: fake.requests.slice(-8), lastError: fake.lastError },
        gui: { pending: [...gui.pending.keys()], applied: gui.totalApplied(), error: gui.error, notices: gui.notices.slice(-8) },
        daemonVersion: report.versions.daemon,
      }, null, 2));
    } catch { /* Diagnostics are best effort. */ }
  }
  await writeFile(report.artifacts.report, JSON.stringify(report, null, 2));
}
function record(list: 'startup' | 'cleanup', step: string, error: unknown) {
  const details = error instanceof Error ? error.message : String(error);
  (report[list] as Array<{ step: string; status: string; details: string }>).push({ step, status: 'failed', details });
  console.error(`[X] ${list} ${step}: ${details}`);
}
function exit(child: ChildProcess): Promise<number | null> {
  return new Promise((ok, fail) => { child.once('error', fail); child.once('exit', ok); });
}

/** One `/v1` client with the daemon's framed JSON: only what this run needs. */
class DaemonSocket {
  private socket!: WebSocket;
  private pending = new Map<string, { resolve: (v: unknown) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  hello: { daemonVersion?: string } | null = null;
  protocolVersion = 0;
  notifications: Array<{ type: string; [key: string]: unknown }> = [];
  private finishes = new Map<string, number>();
  private finishBeforePrompt = new Map<string, number>();

  constructor(readonly address: string, readonly token: string) {
    this.socket = new WebSocket(address);
    this.socket.onopen = () => {};
    this.socket.onmessage = (event: MessageEvent) => {
      let message: { type: string; requestId?: string; outcome?: unknown; daemonVersion?: string };
      try { message = JSON.parse(String(event.data)); } catch { return; }
      if (message.type === 'hello') this.hello = { daemonVersion: message.daemonVersion };
      const notice = message as { type: string; sessionId?: string; runtimeId?: string; event?: { kind: string } };
      if (notice.type === 'event' && notice.event?.kind === 'turnFinished') {
        const key = `${notice.sessionId}/${notice.runtimeId}`;
        this.finishes.set(key, (this.finishes.get(key) ?? 0) + 1);
      }
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
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`could not reach ${this.address}`)), timeoutMs);
      this.socket.onopen = () => { clearTimeout(timer); resolve(); };
      this.socket.onerror = () => { clearTimeout(timer); reject(new Error(`could not reach ${this.address}`)); };
    });
    this.send({ type: 'hello', protocolVersion: this.protocolVersion, token: this.token, clientId: randomUUID(), resumeFrom: [] });
    await poll('daemon hello', () => this.hello, h => h !== null, timeoutMs);
  }

  private send(message: unknown) { this.socket.send(JSON.stringify(message)); }

  request(sessionId: string, runtimeId: string, command: unknown, requestId = randomUUID(), timeoutMs = INVOKE_TIMEOUT_MS): Promise<{ status: string; payload?: { type: string; [key: string]: unknown } }> {
    let timer: ReturnType<typeof setTimeout>;
    const outcome = new Promise<{ status: string; payload?: { type: string; [key: string]: unknown } }>((resolve, reject) => {
      timer = setTimeout(() => { this.pending.delete(requestId); reject(new Error(`the daemon did not answer ${requestId} within ${timeoutMs} ms`)); }, timeoutMs);
      this.pending.set(requestId, { resolve: resolve as (v: unknown) => void, reject, timer });
    });
    this.send({ type: 'request', requestId, sessionId, runtimeId, command });
    return outcome;
  }

  control(message: unknown) { this.send(message); }

  beginTurn(sessionId: string, runtimeId: string) {
    const key = `${sessionId}/${runtimeId}`;
    this.finishBeforePrompt.set(key, this.finishes.get(key) ?? 0);
  }

  async waitForTurn(sessionId: string, runtimeId: string) {
    const key = `${sessionId}/${runtimeId}`;
    await poll('the real driver settled the tested turn', () => this.finishes.get(key) ?? 0,
      count => count > (this.finishBeforePrompt.get(key) ?? 0), 20_000);
  }

  close() {
    ownedSockets.delete(this);
    try { this.socket.close(); } catch { /* Already closed. */ }
  }
}

function isolatedEnv(extra: Record<string, string>): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (key.toUpperCase().startsWith('WEBVIEW2_')) continue;
    // Never leak an outer OpenCode config/provider selection into the run.
    if (key.startsWith('OPENCODE_')) continue;
    if (/(?:API_KEY|APIKEY|ACCESS_TOKEN|AUTH_TOKEN|SECRET_KEY)$/i.test(key)) continue;
    if (key === 'FINTWIND_BROWSER_TOOL_ADDRESS' || key === 'FINTWIND_BROWSER_TOOL_TOKEN') continue;
    env[key] = value;
  }
  Object.assign(env, {
    OPENCODE_CONFIG_DIR: opencodeConfigDir,
    XDG_CONFIG_HOME: xdgConfig,
    XDG_DATA_HOME: xdgData,
    XDG_CACHE_HOME: xdgCache,
    XDG_STATE_HOME: join(output, 'xdg', 'state'),
    OPENCODE_DB: join(xdgData, 'opencode.db'),
    OPENCODE_DISABLE_AUTOUPDATE: '1',
    OPENCODE_DISABLE_FILEWATCHER: '1',
    OPENCODE_DISABLE_PROJECT_CONFIG: '1',
    OPENCODE_CONFIG_PROJECT_DISABLE: '1',
    NO_PROXY: noProxy,
    no_proxy: noProxy,
  }, extra);
  return env;
}

async function opencodeVersion(binary: string): Promise<string> {
  const child = spawn(binary, ['--version'], { shell: false, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: isolatedEnv({}) });
  let out = '';
  child.stdout!.on('data', bytes => { out += String(bytes); });
  child.stderr!.on('data', bytes => { out += String(bytes); });
  const code = await deadline(exit(child), 'opencode --version', 30_000);
  if (code !== 0) throw new Error(`opencode --version exited ${code}: ${out.slice(0, 200)}`);
  return out.split(/\r?\n/).map(line => line.trim()).find(line => line.length > 0) ?? 'unknown';
}

const fakeHandle = startFakeProvider();
const fake = fakeHandle.provider;
const fakeBaseUrl = fakeHandle.baseUrl;
let gui = new GuiProtocolOwner();
let client: DaemonSocket | undefined;
let pumpTimer: ReturnType<typeof setInterval> | undefined;
let turnSeq = 0;
/** Set once the ready line is parsed; `openSocket` builds fresh clients from these. */
let daemonWsAddress = '';
let daemonToken = '';

/** Share one page for (sessionId, runtimeId) through the simulated GUI. */
function sharePage(sessionId: string, runtimeId: string, pageId: string, grantId: string, url: string, title: string) {
  gui.sharePages([{ scope: { sessionId, runtimeId, pageId, grantId }, url, title }]);
}

async function prompt(client: DaemonSocket, sessionId: string, runtimeId: string, marker: string) {
  client.beginTurn(sessionId, runtimeId);
  const outcome = await client.request(sessionId, runtimeId, { type: 'prompt', prompt: `E2E_BROWSER_TOOLS ${marker}` }, randomUUID(), 20_000);
  assert(outcome.status === 'ok', `the prompt to ${marker} must be accepted: ${JSON.stringify(outcome)}`);
}

/** Run a non-gated turn to completion and return the recorded tool results. */
async function runTurn(client: DaemonSocket, sessionId: string, runtimeId: string, plan: PlanStep[]): Promise<{ marker: string; results: typeof fake.toolResults; turnCalls: number }> {
  if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
  const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
  fake.plan_install(plan, marker);
  await prompt(client, sessionId, runtimeId, marker);
  const expected = plan.length;
  await poll(`${marker} reached its final answer`, () => fake.turnCalls >= expected + 1 && fake.toolResults.length === expected, ok => ok, INVOKE_TIMEOUT_MS + 15_000);
  await client.waitForTurn(sessionId, runtimeId);
  report.fixture.turns.push({ marker, toolCalls: expected, turnCalls: fake.turnCalls, results: fake.toolResults.map(r => ({ tool: r.tool, source: r.source })) });
  return { marker, results: fake.toolResults.slice(), turnCalls: fake.turnCalls };
}

/** The distinct tool names OpenCode declared to the provider across requests. */
function providerToolNames(): string[] {
  const names = new Set<string>();
  for (const request of fake.requests) for (const name of request.toolNames) names.add(name);
  return [...names];
}

/** The plugin activated, so `ctx.tool.list()` held the sixteen tools. This asks
 *  the separate question the daemon depends on: did OpenCode put every
 *  `fintwind_browser_*` tool into the executable snapshot it sent to the
 *  provider? */
function browserToolsReachedModel(): boolean {
  return fake.requests.some(request => FINTWIND_BROWSER_TOOL_NAMES
    .every(name => request.toolNames.includes(name)));
}

/** OpenCode's own failure strings. A refusal that matches one of these was
 *  produced inside the plugin process and proves nothing about the daemon. */
const PLUGIN_LOCAL_FAILURES = [
  'unavailable in this process',
  'no live OpenCode session context',
  'No action was issued',
  'Invalid browser tool',
  'Browser bridge connection failed',
  'Browser bridge disconnected',
  'Browser call cancelled',
  'Browser call timed out',
  'Give either the snapshot ref or a CSS selector',
  'Give the element ref from the latest snapshot',
];

/** The precise finding when the model never received the browser tools. */
function pluginToolDiagnosis(): string {
  const seen = providerToolNames();
  return `OpenCode did not expose all ${FINTWIND_BROWSER_TOOL_NAMES.length} direct fintwind_browser_* tools in the model's executable snapshot. The provider saw only: [${seen.join(', ')}]. Registration, activation and Code Mode visibility are separate checks; this evidence does not establish a root cause.`;
}

/** The refusal came from the daemon rather than from the plugin itself. */
function isDaemonRefusal(message: string): boolean {
  return !PLUGIN_LOCAL_FAILURES.some(marker => message.includes(marker));
}

try {
  report.versions.opencode = await opencodeVersion(opencodeBinary);
  report.startup.push({ step: 'opencode version', status: 'ok', details: `${report.versions.opencode} at ${opencodeBinary}` });

  if (!args.has('--skip-build')) {
    const build = spawn('cargo', ['build', '--locked', '--package', 'fintwind-core', '--example', 'browser_tools_opencode', '--target-dir', EXAMPLE_TARGET_DIR], { cwd: root, shell: false, stdio: 'inherit', windowsHide: true });
    try { assert.equal(await deadline(exit(build), 'example build', BUILD_LIMIT_MS), 0); }
    finally { if (build.exitCode === null) build.kill(); }
  }
  const exampleExe = join(root, EXAMPLE_TARGET_DIR, 'debug', 'examples', 'browser_tools_opencode.exe');
  await readFile(exampleExe);
  report.versions.exampleSha256 = createHash('sha256').update(await readFile(exampleExe)).digest('hex');
  budgetStartedAt = Date.now();

  await readFile(opencodeBinary);
  report.startup.push({ step: 'fake provider', status: 'ok', details: fakeBaseUrl });

  // The provider config OpenCode merges with the daemon-injected plugin. No
  // credentials to a real service exist anywhere in this object.
  //
  // Registration and direct model visibility are separate behaviors. These
  // fixture permissions do not bypass the browser owner's per-action approval.
  const BROWSER_TOOLS = [...FINTWIND_BROWSER_TOOL_NAMES];
  const opencodeConfigContent = JSON.stringify({
    $schema: 'https://opencode.ai/config.json',
    model: 'fixture/fixture-model',
    small_model: 'fixture/fixture-model',
    disabled_providers: [],
    tools: Object.fromEntries(BROWSER_TOOLS.map(name => [name, true])),
    permission: Object.fromEntries(BROWSER_TOOLS.map(name => [name, 'allow'])),
    provider: {
      fixture: {
        npm: '@ai-sdk/openai-compatible',
        name: 'Fintwind E2E Fixture',
        options: { apiKey: 'e2e-not-a-real-key', baseURL: fakeBaseUrl },
        models: {
          'fixture-model': {
            name: 'Fixture Model',
            tool_call: true,
            attachment: false,
            limit: { context: 128000, output: 8192 },
          },
        },
      },
    },
  });

  // Spawn the real daemon. It inherits the fake-provider config and the
  // isolated XDG/OpenCode directories, and writes its state only under `state/`.
  let readyLine = '';
  daemon = spawn(exampleExe, [], {
    cwd: root,
    shell: false,
    windowsHide: true,
    stdio: ['ignore', 'pipe', 'pipe'],
    env: isolatedEnv({
      FINTWIND_BROWSER_TOOLS_E2E_STATE: stateDir,
      OPENCODE_CONFIG_CONTENT: opencodeConfigContent,
    }),
  });
  daemon.stdout!.on('data', bytes => { readyLine += String(bytes); });
  daemon.stderr!.on('data', bytes => { const text = String(bytes); exampleStdout += text; process.stderr.write(`[daemon] ${text}`); });
  if (daemon.exitCode !== null) throw new Error(`the daemon exited during startup: ${daemon.exitCode}`);
  await poll('daemon ready line', () => readyLine.includes('browser-tools-opencode') && readyLine.includes('\n'), ok => ok, 30_000);
  let ready: { kind: string; address: string; token: string; protocolVersion: number } | undefined;
  try { ready = JSON.parse(readyLine.split('\n').find(line => line.includes('browser-tools-opencode'))!); }
  catch { throw new Error('The daemon did not return a valid ready line.'); }
  assert(ready && ready.address.startsWith('127.0.0.1:'), 'the daemon listener is loopback only');
  assert(Number.isInteger(ready.protocolVersion) && ready.protocolVersion > 0, 'the daemon prints a protocol version');
  report.versions.protocol = ready.protocolVersion;
  daemonWsAddress = `ws://${ready.address}/v1`;
  daemonToken = ready.token;
  report.startup.push({ step: 'daemon', status: 'ok', details: daemonWsAddress });

  client = new DaemonSocket(daemonWsAddress, daemonToken);
  ownedSockets.add(client);
  client.protocolVersion = ready.protocolVersion;
  await client.connect();
  report.versions.daemon = client.hello?.daemonVersion ?? '';
  assert(report.versions.daemon.length > 0, 'the daemon reports its version');

  // The new browser actions (`open`, `scroll`, element refs) ride the desktop
  // protocol, not the private tool wire, so the built example must already
  // speak the bumped version. The plugin never guesses: it states tool wire
  // version 1 and the daemon either answers 1 or rejects.
  await check('the daemon speaks the desktop protocol the new browser actions need', 'real daemon ready line + real /v1 hello', async () => {
    assert(Number.isInteger(ready!.protocolVersion) && ready!.protocolVersion >= 9,
      `the built example must speak desktop protocol 9 or newer for open/scroll/element refs; its ready line reported ${ready!.protocolVersion}`);
    return `desktop protocol ${ready!.protocolVersion}; the private /v1/browser-tools wire stays at version 1 and nothing here guesses a version.`;
  });

  // ---------------------------------------------------------------------------
  // Two Fintwind sessions on one OpenCode server; the second proves native
  // sessions do not share scope. Each gets its own isolated workspace dir.
  // ---------------------------------------------------------------------------
  const sessionA = randomUUID();
  const runtimeA = randomUUID();
  const sessionB = randomUUID();
  const runtimeB = randomUUID();
  const pageA = randomUUID();
  const grantA = randomUUID();
  const pageUrlA = `http://127.0.0.1/fixture-alpha?run=${runId}`;
  const pageTitleA = 'Fixture Alpha';
  /** One opaque element reference, in the snapshot's own `ref:UUID:ordinal` form. */
  const elementRefA = `ref:${randomUUID()}:1`;

  const START_OPTIONS = {
    binary: opencodeBinary,
    cwd: workspaceDir,
    mode: 'fullAccess',
    interactionMode: 'build',
    model: 'fixture/fixture-model',
    reasoningEffort: null,
    serviceTier: null,
    contextWindow: null,
    agentPreset: null,
    providerCursor: null,
  };

  const start = async (sessionId: string, runtimeId: string, cwd = workspaceDir) => {
    const outcome = await client!.request(sessionId, runtimeId, { type: 'start', options: { ...START_OPTIONS, cwd } }, randomUUID(), 90_000);
    assert(outcome.status === 'ok', `the runtime must start: ${JSON.stringify(outcome)}`);
    return outcome;
  };

  await check('the private OpenCode session starts (' + report.versions.opencode + ')', 'real opencode + real driver, activation not yet proven', async () => {
    await start(sessionA, runtimeA);
    report.startup.push({ step: 'session A', status: 'ok', details: `${sessionA.slice(0, 8)}/${runtimeA.slice(0, 8)}` });
    return 'The real driver started a private OpenCode session. A subsequent tool round-trip must prove browser activation.';
  });

  // The simulated GUI publishes one page for session A's runtime. This is the
  // only page; session B publishes nothing.
  await check('the simulated GUI shares exactly one page for session A', 'simulated GUI -> real broker', async () => {
    await gui.connect(daemonWsAddress, daemonToken, ready!.protocolVersion);
    sharePage(sessionA, runtimeA, pageA, grantA, pageUrlA, pageTitleA);
    await poll('the broker accepted the explicit share', async () => {
      const response = await client!.request(sessionA, runtimeA, { type: 'browserList' });
      return response.payload?.value as Array<{ scope: { pageId: string; grantId: string } }> | undefined;
    }, pages => !!pages?.some(page => page.scope.pageId === pageA && page.scope.grantId === grantA));
    return `one page ${pageA.slice(0, 8)} shared under ${sessionA.slice(0, 8)}`;
  });

  // Background pump: applies the current policy to every pending GUI request.
  pumpTimer = setInterval(() => { try { gui.pump(); } catch { /* transient */ } }, 80);
  // Observe non-mutating snapshots automatically; hold mutations until a check
  // decides. This default is what makes "before approval nothing happened" true.
  gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'hold');

  await check('a list returns this session\'s shared page, tagged as an untrusted source', 'real plugin+registry+broker -> fake provider', async () => {
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    // The provider records a tool result whether the tool executed or was
    // reported unavailable, so this settles quickly either way.
    await poll('the list turn produced a tool result', () => fake.toolResults.some(r => r.tool === 'fintwind_browser_list'), ok => ok, INVOKE_TIMEOUT_MS)
      .catch(() => { /* Analyzed below whether or not a result landed. */ });
    if (!browserToolsReachedModel()) {
      const finding = pluginToolDiagnosis();
      if (!report.findings.includes(finding)) report.findings.push(finding);
      throw new Error(finding);
    }
    await poll('the list turn reached its final answer', () => fake.turnCalls >= 2, ok => ok, INVOKE_TIMEOUT_MS + 15_000);
    await client!.waitForTurn(sessionA, runtimeA);
    const list = fake.toolResults.find(r => r.tool === 'fintwind_browser_list')!;
    assert.equal(fake.turnCalls, 2, `list is one deterministic round trip (emit + final), got ${fake.turnCalls}`);
    assert(!list.browserError, `the executed list must not error: ${list.raw.slice(0, 200)}`);
    assert.equal(list.source, 'untrusted_shared_browser_page', `list must be tagged: ${list.raw.slice(0, 200)}`);
    const pages = list.value as Array<{ pageId: string; grantId: string; url: string; title: string }>;
    assert.equal(pages.length, 1, `one shared page, got ${pages.length}`);
    assert.equal(pages[0]!.pageId, pageA, 'the list returns the shared page id');
    assert.equal(pages[0]!.grantId, grantA, 'the list returns the shared grant id');
    assert.equal(pages[0]!.url, pageUrlA);
    assert.equal(pages[0]!.title, pageTitleA);
    report.fixture.turns.push({ marker, toolCalls: 1, turnCalls: fake.turnCalls, results: [{ tool: 'fintwind_browser_list', source: list.source }] });
    return 'list -> the model saw exactly the one shared page, untrusted-tagged; the plugin tool executed in the real session context.';
  });

  if (report.checks.at(-1)?.status !== 'passed') {
    throw new Error('The required list round-trip failed; stop rather than running actions against an unverified binding.');
  }

  // The transform removal is order-dependent; the per-request context hook is
  // not. Both have to hold, or the model still reaches for a desktop browser
  // tool that can never connect to Fintwind.
  await check(`every model snapshot declares the ${FINTWIND_BROWSER_TOOL_NAMES.length} fintwind tools and no built-in browser tool`, 'real OpenCode tool registry + context hook -> provider snapshot', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    const seen = providerToolNames();
    const builtins = seen.filter(name => BUILTIN_BROWSER_TOOL_IDS.includes(name));
    assert.equal(builtins.length, 0,
      `the built-in desktop browser tools must not reach the model: [${builtins.join(', ')}]`);
    for (const request of fake.requests.filter(r => r.real)) {
      const missing = FINTWIND_BROWSER_TOOL_NAMES.filter(name => !request.toolNames.includes(name));
      assert.equal(missing.length, 0,
        `one request declared only [${request.toolNames.join(', ')}]; missing [${missing.join(', ')}]`);
    }
    report.fixture.providerToolNames = seen;
    report.fixture.builtinBrowserToolsSeen = builtins;
    return `${seen.length} tools reached the model: all ${FINTWIND_BROWSER_TOOL_NAMES.length} fintwind_browser_* directly, and none of the ${BUILTIN_BROWSER_TOOL_IDS.length} built-in desktop browser ids.`;
  });

  await check('every model request carries the Fintwind in-app browser instruction', 'real plugin context hook -> fake provider capture', async () => {
    assert(fake.requests.length > 0, 'the provider recorded at least one request');
    // The agent loop declares tools. Title/summary auxiliary requests carry
    // none and, per the V2 plugin docs, have their own `title` hook rather
    // than the `context` hook this instruction rides — so they are counted,
    // not failed.
    const agentLoop = fake.requests.filter(request => request.toolNames.length > 0);
    const auxiliary = fake.requests.length - agentLoop.length;
    assert(agentLoop.length > 0, 'the provider recorded at least one agent-loop request');
    const carried = agentLoop.filter(request => request.fintwindBrowserInstruction).length;
    report.fixture.fintwindBrowserInstructionRequests = carried;
    report.fixture.auxiliaryProviderRequests = auxiliary;
    assert.equal(carried, agentLoop.length,
      `every agent-loop request must carry the instruction; ${carried} of ${agentLoop.length} did`);
    return `${carried} agent-loop requests carried the instruction ("You are in Fintwind ...") and ${auxiliary} tool-less auxiliary request(s) were the documented title path; the prompt body itself is never written to an artifact.`;
  });

  await check('a snapshot reaches the model wrapped as an untrusted source, round-trip intact', 'real plugin+registry+broker -> fake provider', async () => {
    // Shaped exactly like the native snapshot script emits: every control
    // carries `ref` and `selector` set to the same opaque token, plus the
    // document scope and the untrusted-content flag.
    const snapshot = {
      url: pageUrlA,
      title: pageTitleA,
      text: 'simulated snapshot marker should reach the provider verbatim',
      controls: [{ tag: 'button', role: 'button', name: 'Count', ref: elementRefA, selector: elementRefA, disabled: false }],
      truncated: true,
      scope: 'main_document',
      untrustedPageContent: true,
      refNonce: elementRefA.slice(4, -2),
    };
    gui.snapshotFor = () => snapshot;
    const { results } = await runTurn(client!, sessionA, runtimeA, [
      { tool: 'fintwind_browser_list' },
      { tool: 'fintwind_browser_snapshot', page: { from: 'list', index: 0 } },
    ]);
    assert.equal(fake.turnCalls, 3, `list+snapshot+final is three provider calls, got ${fake.turnCalls}`);
    const snap = results[1]!;
    assert.equal(snap.source, 'untrusted_shared_browser_page', `snapshot must be untrusted-tagged: ${snap.raw.slice(0, 200)}`);
    assert.deepEqual(snap.value, snapshot, 'the exact snapshot value reached the provider');
    // The snapshot argument (pageId/grantId) was built from the real list result.
    assert.equal(snap.tool, 'fintwind_browser_snapshot');
    return 'list fed the real pageId/grantId into snapshot; the model received the GUI value wrapped as untrusted.';
  });

  await check('a click is gated: nothing happens until approval, then the run applies it', 'real plugin+registry+broker -> simulated GUI', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    const before = gui.totalApplied();
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_click', page: { from: 'list', index: 0 }, target: { selector: '#count' } }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the click reached the GUI as a pending approval', () => gui.hasPending('click', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    // Held, not approved: the plugin must not have mutated anything, and the
    // fake provider has emitted list + click but not yet seen the click result.
    assert.equal(gui.totalApplied(), before, 'no mutation before approval');
    assert.equal(gui.page(pageA)!.appliedCount, before, 'the page counter is unchanged before approval');
    // Approve now.
    gui.policy = () => 'approve';
    await poll('the approved click reaches the model', () => fake.toolResults.some(r => r.tool === 'fintwind_browser_click'), ok => ok, INVOKE_TIMEOUT_MS);
    await poll('the click turn reaches its final answer', () => fake.turnCalls >= 3 && fake.toolResults.length === 2, ok => ok, INVOKE_TIMEOUT_MS);
    await client!.waitForTurn(sessionA, runtimeA);
    const click = fake.toolResults.find(r => r.tool === 'fintwind_browser_click')!;
    assert.equal(click.source, 'untrusted_shared_browser_page', `approved click is wrapped: ${click.raw.slice(0, 200)}`);
    assert.deepEqual(click.value, { applied: true }, 'the GUI applied the click');
    assert.equal(gui.page(pageA)!.appliedCount, before + 1, 'the click applied exactly once after approval');
    return 'held click applied nothing; after approval the mutation reached the GUI exactly once.';
  });

  await check('a rejected click mutates nothing and reports as an error to the model', 'real plugin+registry+broker -> simulated GUI', async () => {
    const before = gui.page(pageA)!.appliedCount;
    gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'reject');
    const { results } = await runTurn(client!, sessionA, runtimeA, [
      { tool: 'fintwind_browser_list' },
      { tool: 'fintwind_browser_click', page: { from: 'list', index: 0 }, target: { selector: '#count' } },
    ]);
    const click = results[1]!;
    assert(click.browserError && /reject/i.test(click.browserError), `the rejected click must error: ${click.raw.slice(0, 200)}`);
    assert.equal(gui.page(pageA)!.appliedCount, before, 'a rejected click did not act');
    return 'the rejection reached the caller as an error and the page counter stayed put.';
  });

  await check('fill is registered and gated the same way (held -> approved applies)', 'real plugin+registry+broker -> simulated GUI', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'hold');
    const before = gui.page(pageA)!.appliedCount;
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_fill', page: { from: 'list', index: 0 }, target: { selector: '#name' }, text: 'e2e-filled' }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the fill reached the GUI as a pending approval', () => gui.hasPending('fill', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    assert.equal(gui.page(pageA)!.appliedCount, before, 'no text typed before approval');
    gui.policy = () => 'approve';
    await poll('the fill turn reaches its final answer', () => fake.turnCalls >= 3 && fake.toolResults.length === 2, ok => ok, INVOKE_TIMEOUT_MS);
    await client!.waitForTurn(sessionA, runtimeA);
    const fill = fake.toolResults.find(r => r.tool === 'fintwind_browser_fill')!;
    assert.equal(fill.source, 'untrusted_shared_browser_page', 'approved fill is wrapped');
    assert.equal(gui.page(pageA)!.appliedCount, before + 1, 'the fill applied exactly once after approval');
    return 'fill waited for approval, then applied exactly once through the bridge.';
  });

  await check('navigate is registered and gated the same way (held -> approved)', 'real plugin+registry+broker -> simulated GUI', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'hold');
    const before = gui.page(pageA)!.appliedCount;
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_navigate', page: { from: 'list', index: 0 }, url: `http://127.0.0.1/fixture-alpha?run=${runId}#n` }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the navigate reached the GUI as a pending approval', () => gui.hasPending('navigate', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    assert.equal(gui.page(pageA)!.appliedCount, before, 'no navigation before approval');
    gui.policy = () => 'approve';
    await poll('the navigate turn reaches its final answer', () => fake.turnCalls >= 3 && fake.toolResults.length === 2, ok => ok, INVOKE_TIMEOUT_MS);
    await client!.waitForTurn(sessionA, runtimeA);
    const nav = fake.toolResults.find(r => r.tool === 'fintwind_browser_navigate')!;
    assert.equal(nav.source, 'untrusted_shared_browser_page', 'approved navigate is wrapped');
    assert.equal(gui.page(pageA)!.appliedCount, before + 1, 'the navigation applied exactly once after approval');
    return 'list, snapshot, click, fill, navigate and scroll are all registered and per-action gated.';
  });

  // `open` is the one action with no page yet, so there is nothing a fixture
  // could answer it with: the plugin sends `{type:"open", url}` over the real
  // private socket and the real daemon either finds a Fintwind window hosting
  // this session's browser or refuses. This run never registers a host, so the
  // refusal text itself is the evidence that the request crossed the bridge.
  await check('fintwind_browser_open really reaches the daemon, which refuses it without a GUI browser host', 'real plugin -> real daemon -> fake provider', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    // A page is shared and the broker is live, so the only thing missing is a
    // registered browser host. Nothing is queued for approval and no page is
    // touched, which is what "reached the daemon and was refused" looks like.
    gui.policy = () => 'hold';
    const before = gui.totalApplied();
    const openUrl = `http://127.0.0.1/fixture-open?run=${runId}`;
    const { results } = await runTurn(client!, sessionA, runtimeA, [{ tool: 'fintwind_browser_open', url: openUrl }]);
    const open = results[0]!;
    assert(open.browserError, `open must be refused while no Fintwind window hosts this session: ${open.raw.slice(0, 300)}`);
    assert(isDaemonRefusal(open.browserError), `the refusal must come from the daemon, not from the plugin: ${open.browserError}`);
    assert(/launcher|host/i.test(open.browserError), `the daemon must name the missing browser launcher: ${open.browserError}`);
    assert.equal(gui.pending.size, 0, 'an unhosted open is not queued for a page owner');
    assert.equal(gui.totalApplied(), before, 'an unhosted open never touches a page');
    return `the plugin's {type:"open"} crossed the real private socket and the daemon answered: ${open.browserError}`;
  });

  await check('fintwind_browser_scroll is gated and applies the exact delta', 'real plugin+registry+broker -> simulated GUI', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'hold');
    const before = gui.page(pageA)!.appliedCount;
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_scroll', page: { from: 'list', index: 0 }, deltaY: -450 }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the scroll reached the GUI as a pending approval', () => gui.hasPending('scroll', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    assert.equal(gui.page(pageA)!.appliedCount, before, 'no scrolling before approval');
    const pending = gui.pendingActions('scroll', pageA)[0];
    assert.equal(pending?.kind, 'scroll', 'the pending action is a scroll');
    if (pending?.kind !== 'scroll') throw new Error('the pending action was not a scroll');
    assert.equal(pending.deltaY, -450, 'the scroll carries the exact requested delta');
    gui.policy = () => 'approve';
    await poll('the scroll turn reaches its final answer', () => fake.turnCalls >= 3 && fake.toolResults.length === 2, ok => ok, INVOKE_TIMEOUT_MS);
    await client!.waitForTurn(sessionA, runtimeA);
    const scroll = fake.toolResults.find(r => r.tool === 'fintwind_browser_scroll')!;
    assert.equal(scroll.source, 'untrusted_shared_browser_page', 'an approved scroll is wrapped');
    assert.deepEqual(scroll.value, { kind: 'scroll', deltaY: -450 }, 'the applied scroll is echoed back to the model');
    assert.equal(gui.page(pageA)!.appliedCount, before + 1, 'the scroll applied exactly once after approval');
    return 'scroll waited for approval, applied exactly once and echoed the exact delta to the model.';
  });

  await check('an out-of-bounds scroll is refused before any page action', 'real plugin execute (or OpenCode schema) -> simulated GUI owner remains untouched', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = () => 'hold';
    const before = gui.totalApplied();
    // Two layers must refuse these, and which one answers first is not the
    // point: the schema bound and the plugin's own bound both refuse before a
    // connection is opened, so nothing can reach the page owner.
    const refusedBy: string[] = [];
    for (const deltaY of [0, 2500, 1.5]) {
      const { results } = await runTurn(client!, sessionA, runtimeA,
        [{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_scroll', page: { from: 'list', index: 0 }, deltaY }]);
      const refusal = String(results.at(-1)?.raw);
      if (refusal.includes('No action was issued')) refusedBy.push(`plugin(${deltaY})`);
      else if (/Expected a value less than or equal to|Expected an integer/.test(refusal)) refusedBy.push(`schema(${deltaY})`);
      else throw new Error(`delta ${deltaY} was not refused explicitly: ${refusal.slice(0, 200)}`);
      assert.equal(gui.pending.size, 0, 'no out-of-bounds scroll reaches approval');
      assert.equal(gui.totalApplied(), before, 'no out-of-bounds scroll is applied');
    }
    return `zero, oversized and fractional scrolls were refused before any page action (${refusedBy.join(' and ')}), without connection errors or retries.`;
  });

  await check('an opaque snapshot element reference drives click instead of a CSS selector', 'real plugin+registry+broker -> simulated GUI', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'hold');
    const snapshot = {
      url: pageUrlA,
      title: pageTitleA,
      text: 'snapshot carrying an opaque element reference',
      controls: [{ tag: 'button', role: 'button', name: 'Count', ref: elementRefA, selector: elementRefA, disabled: false }],
      truncated: true,
      scope: 'main_document',
      untrustedPageContent: true,
      refNonce: elementRefA.slice(4, -2),
    };
    gui.snapshotFor = () => snapshot;
    const before = gui.page(pageA)!.appliedCount;
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([
      { tool: 'fintwind_browser_list' },
      { tool: 'fintwind_browser_snapshot', page: { from: 'list', index: 0 } },
      { tool: 'fintwind_browser_click', page: { from: 'list', index: 0 }, target: { ref: elementRefA } },
    ], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the ref click reached the GUI as a pending approval', () => gui.hasPending('click', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    const pending = gui.pendingActions('click', pageA)[0];
    assert.equal(pending?.kind, 'click', 'the pending action is a click');
    if (pending?.kind !== 'click') throw new Error('the pending action was not a click');
    // The wire has one selector field. The opaque ref rides it, exactly as the
    // native owner's CSS fallback does, so no second protocol field exists.
    assert.equal(pending.selector, elementRefA, 'the click carried the opaque snapshot reference');
    assert.equal(pending.selector?.startsWith('ref:'), true, 'the value is the opaque ref, not a CSS selector');
    assert.equal(gui.page(pageA)!.appliedCount, before, 'no click before approval');
    gui.policy = () => 'approve';
    await poll('the ref click turn reaches its final answer', () => fake.turnCalls >= 4 && fake.toolResults.length === 3, ok => ok, INVOKE_TIMEOUT_MS);
    await client!.waitForTurn(sessionA, runtimeA);
    // The snapshot reached the model verbatim, so the ref it was given is the
    // one the real page exposed — not a selector the plugin invented.
    assert.deepEqual(fake.toolResults.find(r => r.tool === 'fintwind_browser_snapshot')!.value, snapshot,
      'the snapshot value reached the provider verbatim');
    const click = fake.toolResults.find(r => r.tool === 'fintwind_browser_click')!;
    assert.equal(click.source, 'untrusted_shared_browser_page', 'the ref click is wrapped');
    assert(!click.browserError, `the ref click must succeed: ${click.raw.slice(0, 200)}`);
    assert.equal(gui.page(pageA)!.appliedCount, before + 1, 'the ref click applied exactly once after approval');
    return `the opaque ref ${elementRefA.slice(0, 20)}... carried the click across the wire as its selector, with no CSS selector and no second protocol field.`;
  });

  await check('a second native session does not cross scope and cannot use session A\'s page', 'real plugin+registry+broker -> fake provider', async () => {
    const secondWorkspace = join(output, 'workspace-b');
    await mkdir(secondWorkspace, { recursive: true });
    await start(sessionB, runtimeB, secondWorkspace);
    report.startup.push({ step: 'session B', status: 'ok', details: `${sessionB.slice(0, 8)}/${runtimeB.slice(0, 8)}` });
    // B lists: its runtime has no shared page, so it must be empty (no fallback
    // to A's page, no current-page guess).
    const list = await runTurn(client!, sessionB, runtimeB, [{ tool: 'fintwind_browser_list' }]);
    assert(Array.isArray(list.results[0]!.value), 'B\'s list value is an array');
    assert.equal((list.results[0]!.value as unknown[]).length, 0, 'an unmapped runtime lists nothing');
    // B tries to use A's page by id: refused, because the resolved scope is
    // (sessionB, runtimeB), which has no such publication.
    const probe = await runTurn(client!, sessionB, runtimeB, [
      { tool: 'fintwind_browser_snapshot', page: { pageId: pageA, grantId: grantA } },
    ]);
    const snap = probe.results[0]!;
    assert(snap.browserError && /no live browser page|not shared|scope/i.test(snap.browserError),
      `session B must be refused for A's page: ${snap.raw.slice(0, 200)}`);
    return 'session B listed nothing and was refused for session A\'s page — two native sessions stay in their own scope.';
  });

  await check('the fake provider is called deterministically and the counter reflects approved mutations only', 'fake provider + simulated GUI counter', async () => {
    gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'hold');
    const before = gui.totalApplied();
    const callsBefore = fake.count();
    const { turnCalls } = await runTurn(client!, sessionA, runtimeA, [
      { tool: 'fintwind_browser_list' },
      { tool: 'fintwind_browser_snapshot', page: { from: 'list', index: 0 } },
    ]);
    // Exactly emit-list, advance-to-snapshot, advance-to-final: three calls.
    assert.equal(turnCalls, 3, `a two-step plan is exactly three provider calls, got ${turnCalls}`);
    assert(fake.count() > callsBefore, 'the fake provider recorded the turn');
    // Nothing was approved here (only a snapshot), so the counter is unchanged.
    assert.equal(gui.totalApplied(), before, 'a snapshot-only turn moves no counter');
    report.fixture.fakeProviderCalls = fake.count();
    return `provider call count deterministic (${turnCalls} for a two-step plan); the UI counter held at ${before}.`;
  });

  await check('oversized UTF-8 fill and JSON-escaped messages fail explicitly without issuing actions', 'real plugin execute -> fake provider; real owner remains untouched', async () => {
    const before = gui!.totalApplied();
    for (const input of [{ text: '界'.repeat(8192), message: '8 KiB UTF-8' },
      { text: '\u0001'.repeat(8192), message: '32 KiB' }]) {
      const { results } = await runTurn(client!, sessionA, runtimeA,
        [{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_fill', page: { from: 'list', index: 0 }, target: { selector: '#name' }, text: input.text }]);
      assert(results.at(-1)?.raw.includes(input.message), 'the model receives an explicit size-limit refusal');
      assert(results.at(-1)?.raw.includes('No action was issued'), 'the failure is not an uncertain connection error');
      assert.equal(gui!.pending.size, 0, 'no oversized action reaches approval');
      assert.equal(gui!.totalApplied(), before, 'no oversized action is applied');
    }
    return 'Multibyte text and JSON expansion were refused before any page action, without connection errors or retries.';
  });

  await check('an interrupt cancels the in-flight browser work and drops the pending approval', 'real driver + real OpenCode interrupt + simulated GUI', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = () => 'hold'; // hold every mutation so the click waits at the GUI
    const before = gui.page(pageA)!.appliedCount;
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_click', page: { from: 'list', index: 0 }, target: { selector: '#count' } }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the click is pending approval', () => gui.hasPending('click', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    assert.equal(gui.page(pageA)!.appliedCount, before, 'nothing changed before the interrupt');
    // Command::Cancel makes the driver POST /api/session/{id}/interrupt; the
    // real OpenCode executor aborts the tool, which aborts context.signal, so
    // the plugin sends its own cancel and the broker removes the request.
    const cancelled = client!.request(sessionA, runtimeA, { type: 'cancel' }, randomUUID(), 20_000);
    await poll('the broker told the GUI to abandon the approval', () => !gui.hasPending('click', pageA), ok => ok, 25_000);
    assert.equal((await cancelled).status, 'ok', 'the driver accepted the real interrupt');
    await client!.waitForTurn(sessionA, runtimeA);
    assert.equal(gui.page(pageA)!.appliedCount, before, 'the interrupted click never applied');
    return 'interrupt cancelled the pending click: the GUI approval was withdrawn and no action occurred.';
  });

  await check('a page-owner disconnect is terminal: the caller does not retry or re-send', 'real plugin no-retry -> simulated GUI disconnect', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = () => 'hold';
    const before = gui.page(pageA)!.appliedCount;
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_click', page: { from: 'list', index: 0 }, target: { selector: '#count' } }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the click is pending approval', () => gui.hasPending('click', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    // The owner drops its own daemon connection, the way closing the app would.
    gui.close();
    await poll('the caller observes the owner disconnect as a tool error', () => fake.toolResults.some(r => r.tool === 'fintwind_browser_click'), ok => ok, INVOKE_TIMEOUT_MS + 15_000);
    await poll('the click turn settles', () => fake.turnCalls >= 3, ok => ok, 10_000);
    await client!.waitForTurn(sessionA, runtimeA);
    const clicks = fake.toolResults.filter(r => r.tool === 'fintwind_browser_click');
    assert.equal(clicks.length, 1, `the caller must not retry the click; recorded ${clicks.length}`);
    assert(clicks[0]!.browserError && /disconnect|owner|gone/i.test(clicks[0]!.browserError),
      `the caller must fail on disconnect, not act: ${clicks[0]!.raw.slice(0, 200)}`);
    assert.equal(gui.totalApplied(), before, 'no action was re-sent after the disconnect');
    return 'owner disconnect ended the call with an error; no reconnect, no re-sent action.';
  });

  // The disconnect scenario above ended the sharing connection on purpose.
  // The remaining scenarios still need a live owner, so reconnect and publish
  // the same page again — a replacement connection, exactly like reopening
  // the app window would be.
  await gui.connect(daemonWsAddress, daemonToken, ready!.protocolVersion);
  sharePage(sessionA, runtimeA, pageA, grantA, pageUrlA, pageTitleA);
  await poll('the replacement owner re-shared the page', async () => {
    const response = await client!.request(sessionA, runtimeA, { type: 'browserList' });
    return response.payload?.value as Array<{ scope: { pageId: string; grantId: string } }> | undefined;
  }, pages => !!pages?.some(page => page.scope.pageId === pageA && page.scope.grantId === grantA));

  // The simulated owner does not run JavaScript: the value that comes back is
  // its `issued` echo, so this run can only prove that an approved evaluate
  // crossed the bridge exactly once — never that arbitrary JS executed.
  await check('fintwind_browser_evaluate is gated like a mutation and the owner answers its issued echo', 'real plugin+registry+broker -> simulated GUI', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'hold');
    const before = gui.page(pageA)!.appliedCount;
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_evaluate', page: { from: 'list', index: 0 }, expression: 'document.title' }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the evaluate reached the GUI as a pending approval', () => gui.hasPending('evaluate', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    assert.equal(gui.page(pageA)!.appliedCount, before, 'no JavaScript ran before approval');
    gui.policy = () => 'approve';
    await poll('the evaluate turn reaches its final answer', () => fake.turnCalls >= 3 && fake.toolResults.length === 2, ok => ok, INVOKE_TIMEOUT_MS);
    await client!.waitForTurn(sessionA, runtimeA);
    const evaluate = fake.toolResults.find(r => r.tool === 'fintwind_browser_evaluate')!;
    assert.equal(evaluate.source, 'untrusted_shared_browser_page', `approved evaluate is wrapped: ${evaluate.raw.slice(0, 200)}`);
    assert.deepEqual(evaluate.value, { kind: 'evaluate', issued: true }, 'the owner answered with its issued echo, not a computed value');
    assert.equal(gui.page(pageA)!.appliedCount, before + 1, 'the evaluate applied exactly once after approval');
    return 'evaluate was held like any mutation; after approval the model saw the simulated owner\'s issued echo as untrusted page data.';
  });

  // The media path is the one result shape that is not a JSON `ok` value: the
  // plugin must turn the daemon's base64 media result into a data-URI file
  // part, and the recorded result must show the model received both parts.
  // The fixture's PNG is a known constant, so the exact bytes are asserted.
  await check('fintwind_browser_screenshot returns a media result the model receives as a file part', 'real plugin+registry+broker -> simulated GUI', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'hold');
    const before = gui.totalApplied();
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_screenshot', page: { from: 'list', index: 0 }, fullPage: true }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the screenshot reached the GUI as a pending approval', () => gui.hasPending('screenshot', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    assert.equal(gui.totalApplied(), before, 'a screenshot is an observation: no mutation before approval');
    const pending = gui.pendingActions('screenshot', pageA)[0];
    assert.equal(pending?.kind, 'screenshot', 'the pending action is a screenshot');
    if (pending?.kind !== 'screenshot') throw new Error('the pending action was not a screenshot');
    assert.equal(pending.fullPage, true, 'the screenshot carries the requested fullPage flag');
    gui.policy = () => 'approve';
    await poll('the screenshot turn reaches its final answer', () => fake.turnCalls >= 3 && fake.toolResults.length === 2, ok => ok, INVOKE_TIMEOUT_MS);
    await client!.waitForTurn(sessionA, runtimeA);
    const shot = fake.toolResults.find(r => r.tool === 'fintwind_browser_screenshot')!;
    assert.equal(shot.source, 'untrusted_shared_browser_page', `the text wrapper must be untrusted-tagged: ${shot.raw.slice(0, 200)}`);
    assert.equal(shot.screenshotMime, 'image/png', `the file part must carry the png mime: ${shot.raw.slice(0, 200)}`);
    // The provider wire (an OpenAI-compatible `image_url`) carries no
    // filename, so the name is only assertable when it survived the hop.
    assert.ok(!shot.screenshotName || shot.screenshotName === 'screenshot.png',
      `an unexpected file name survived the hop: ${shot.screenshotName}`);
    assert(shot.screenshotDataUri !== null && shot.screenshotDataUri.startsWith('data:image/png;base64,'),
      `the image must reach the model as a data-URI file part: ${shot.raw.slice(0, 200)}`);
    assert.equal(shot.screenshotDataUri, `data:image/png;base64,${PNG_1X1_TRANSPARENT_BASE64}`,
      'the exact fixture PNG bytes survived the media round trip');
    assert.equal(gui.totalApplied(), before, 'an approved screenshot moves no mutation counter');
    return 'the approved screenshot reached the model as an image file part — exact fixture PNG bytes in data-URI form with the untrusted text wrapper — and the mutation counter never moved.';
  });

  await check('fintwind_browser_press carries the exact key combination and the approved echo returns it', 'real plugin+registry+broker -> simulated GUI', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'hold');
    const before = gui.page(pageA)!.appliedCount;
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_press', page: { from: 'list', index: 0 }, target: { selector: '#q' }, key: 'Control+A' }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the press reached the GUI as a pending approval', () => gui.hasPending('press', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    const pending = gui.pendingActions('press', pageA)[0];
    assert.equal(pending?.kind, 'press', 'the pending action is a press');
    if (pending?.kind !== 'press') throw new Error('the pending action was not a press');
    assert.equal(pending.key, 'Control+A', 'the press carries the exact requested key combination');
    assert.equal(gui.page(pageA)!.appliedCount, before, 'no keypress before approval');
    gui.policy = () => 'approve';
    await poll('the press turn reaches its final answer', () => fake.turnCalls >= 3 && fake.toolResults.length === 2, ok => ok, INVOKE_TIMEOUT_MS);
    await client!.waitForTurn(sessionA, runtimeA);
    const press = fake.toolResults.find(r => r.tool === 'fintwind_browser_press')!;
    assert.equal(press.source, 'untrusted_shared_browser_page', 'an approved press is wrapped');
    assert.deepEqual(press.value, { kind: 'press', key: 'Control+A' }, 'the applied press echoes the exact key back to the model');
    assert.equal(gui.page(pageA)!.appliedCount, before + 1, 'the press applied exactly once after approval');
    return 'the press waited for approval with its exact key combination intact, then echoed it to the model once applied.';
  });

  // The plugin has no local click_at bound, so the tool schema (`minimum: 0`,
  // `maximum: 8192` on both integer coordinates) is the layer that must
  // refuse; either way the refusal has to be explicit and nothing may reach
  // the page owner or its ledger.
  await check('an out-of-range click_at is refused before any page action', 'real plugin execute (or OpenCode schema) -> simulated GUI owner remains untouched', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = () => 'hold';
    const before = gui.totalApplied();
    const refusedBy: string[] = [];
    for (const coords of [{ x: 9000, y: 10 }, { x: 10, y: -1 }, { x: 10.5, y: 10 }]) {
      const { results } = await runTurn(client!, sessionA, runtimeA,
        [{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_click_at', page: { from: 'list', index: 0 }, x: coords.x, y: coords.y }]);
      const refusal = String(results.at(-1)?.raw);
      if (refusal.includes('No action was issued')) refusedBy.push(`plugin(${coords.x},${coords.y})`);
      else if (/Expected a value less than or equal to|Expected a value greater than or equal to|Expected an integer/.test(refusal)) refusedBy.push(`schema(${coords.x},${coords.y})`);
      else throw new Error(`click_at (${coords.x}, ${coords.y}) was not refused explicitly: ${refusal.slice(0, 200)}`);
      assert.equal(gui.pending.size, 0, 'no out-of-range click_at reaches approval');
      assert.equal(gui.totalApplied(), before, 'no out-of-range click_at is applied');
    }
    return `oversized, negative and fractional click_at coordinates were refused before any page action (${refusedBy.join(' and ')}), without connection errors or retries.`;
  });

  await check('fintwind_browser_close is gated and the page owner answers with closed', 'real plugin+registry+broker -> simulated GUI', async () => {
    if (!browserToolsReachedModel()) throw new Error(`blocked: ${pluginToolDiagnosis()}`);
    gui.policy = (action) => (action.kind === 'snapshot' ? 'approve' : 'hold');
    const before = gui.page(pageA)!.appliedCount;
    const marker = `T${(turnSeq += 1)}_${randomUUID().slice(0, 8)}`;
    fake.plan_install([{ tool: 'fintwind_browser_list' }, { tool: 'fintwind_browser_close', page: { from: 'list', index: 0 } }], marker);
    await prompt(client!, sessionA, runtimeA, marker);
    await poll('the close reached the GUI as a pending approval', () => gui.hasPending('close', pageA), ok => ok, INVOKE_TIMEOUT_MS);
    assert.equal(gui.page(pageA)!.appliedCount, before, 'no page was closed before approval');
    gui.policy = () => 'approve';
    await poll('the close turn reaches its final answer', () => fake.turnCalls >= 3 && fake.toolResults.length === 2, ok => ok, INVOKE_TIMEOUT_MS);
    await client!.waitForTurn(sessionA, runtimeA);
    const close = fake.toolResults.find(r => r.tool === 'fintwind_browser_close')!;
    assert.equal(close.source, 'untrusted_shared_browser_page', 'an approved close is wrapped');
    assert.deepEqual(close.value, { closed: true }, 'the simulated owner answered the close');
    assert.equal(gui.page(pageA)!.appliedCount, before + 1, 'the close applied exactly once after approval');
    return 'close waited for approval and the simulated owner answered {closed:true} — a real owner would end the page and revoke its scope.';
  });

  report.startup.push({ step: 'checks complete', status: 'ok', details: `${report.checks.filter(c => c.status === 'passed').length} passed` });
} catch (error) {
  report.errors.push(error instanceof Error ? error.stack ?? error.message : String(error));
  console.error('[X] E2E could not complete:', error);
} finally {
  // A run that threw early executed fewer checks than it claims; say so rather
  // than letting the check list look complete.
  report.fixture.providerToolNames = providerToolNames();
  report.fixture.builtinBrowserToolsSeen = providerToolNames().filter(name => BUILTIN_BROWSER_TOOL_IDS.includes(name));
  if (report.checks.length !== EXPECTED_CHECKS) {
    report.errors.push(`the run executed ${report.checks.length} of ${EXPECTED_CHECKS} checks, so the acceptance scope was not fully covered`);
  }
  // Reap the daemon first: its shutdown calls opencode_pool::shutdown_all,
  // which is what kills the private `opencode serve` children. Doing this
  // before the in-process teardown means a throw below cannot orphan them.
  await Promise.allSettled(inflight.map(entry => entry.promise));
  if (daemon && daemon.exitCode === null && daemon.signalCode === null) {
    try {
      const shutdown = openSocket();
      await shutdown.connect();
      shutdown.control({ type: 'shutdown' });
      await deadline(exit(daemon), 'daemon shutdown', 10_000);
      report.cleanup.push({ step: 'daemon shutdown', status: 'ok', details: 'ClientMessage::Shutdown' });
      shutdown.close();
    } catch (error) {
      if (daemon.pid && daemon.exitCode === null && daemon.signalCode === null) {
        const kill = spawn('taskkill', ['/PID', String(daemon.pid), '/T', '/F'], { shell: false, windowsHide: true, stdio: 'ignore' });
        try {
          assert.equal(await deadline(exit(kill), 'daemon taskkill', 8_000), 0);
          report.cleanup.push({ step: 'daemon taskkill', status: 'ok', details: `pid ${daemon.pid} (tree, reclaiming opencode serve)` });
        } catch (killError) { record('cleanup', 'daemon taskkill', killError); }
      } else { record('cleanup', 'daemon shutdown', error); }
    }
  }
  // Best-effort in-process teardown; guarded so no step can skip another.
  try { if (pumpTimer) clearInterval(pumpTimer); } catch { /* ignore */ }
  try { fake.plan_disarm(); } catch { /* ignore */ }
  try { gui.close(); } catch { /* ignore */ }
  for (const socket of [...ownedSockets]) { try { socket.close(); } catch { /* ignore */ } }
  try { fakeHandle.stop(); report.cleanup.push({ step: 'fake provider stop', status: 'ok', details: 'loopback stub stopped' }); }
  catch (error) { record('cleanup', 'fake provider stop', error); }
  if (interrupted && !report.errors.includes('E2E interrupted.')) report.errors.push('E2E interrupted.');

  // A token must never survive into any artifact. The ready line's token and the
  // injected browser-tool credential live only in memory; double-check the
  // reports dir holds none.
  report.finishedAt = new Date().toISOString();
  report.fixture.fakeProviderCalls = fake.count();
  report.status = report.errors.length || report.checks.some(check => check.status === 'failed')
    || report.cleanup.some(step => step.status === 'failed') ? 'failed' : 'passed';
  await writeFile(report.artifacts.report, JSON.stringify(report, null, 2));

  // Token leak guard: scan every artifact this run wrote.
  const secretScan = await scanForSecrets(output, readyToken());
  if (secretScan) report.errors.push(secretScan);
  report.status = report.errors.length ? 'failed' : report.status;
  await writeFile(report.artifacts.report, JSON.stringify(report, null, 2));
  console.log(`Report: ${report.artifacts.report}`);
  process.exitCode = report.status === 'passed' ? 0 : 1;
}

function openSocket(): DaemonSocket {
  const socket = new DaemonSocket(daemonWsAddress, daemonToken);
  socket.protocolVersion = report.versions.protocol;
  ownedSockets.add(socket);
  return socket;
}

function readyToken(): string {
  return daemonToken;
}

async function scanForSecrets(dir: string, token: string): Promise<string | null> {
  let entries;
  try { entries = await readdir(dir); } catch { return null; }
  for (const entry of entries) {
    const path = join(dir, entry);
    const info = await stat(path);
    if (info.isDirectory()) {
      const found = await scanForSecrets(path, token);
      if (found) return found;
      continue;
    }
    if (!/\.(?:json|jsonl|log|txt|md)$/i.test(entry)) continue;
    try {
      const text = await readFile(path, 'utf8');
      if (token && text.includes(token)) return `an artifact leaked the daemon token: ${entry}`;
      if (/FINTWIND_BROWSER_TOOL_TOKEN["'\s:=]+[0-9a-f]{64}/.test(text)) {
        return `an artifact appears to carry a browser tool credential: ${entry}`;
      }
    } catch { return `an artifact could not be checked for credentials: ${entry}`; }
  }
  return null;
}
