/**
 * Native behavioral E2E for the Fintwind OpenCode browser tools: the **real**
 * Step 5 Preview model and the **real** WebView2 PoC host.
 *
 * What is real, and what this run is not honest about hiding:
 * - daemon / driver / OpenCode serve / private `/v1/browser-tools` / broker:
 *   the production code, started by the real `FintwindBackend` example.
 * - the model: the user-authorized StepFun `step-5-preview` (low variant),
 *   reached through Fintwind's private OpenCode server with an isolated,
 *   read-only credential hand-off. No project code and no real web content is
 *   ever sent: the only pages are the loopback fixture.
 * - the GUI: the real native WebView2 PoC host (`--browser-poc`), which is a
 *   test harness host, not the product surface. Playwright is used only to
 *   read the fixture DOM and to type fixture values; every action under test is
 *   the daemon's reverse RPC into the native adapter, gated by the real
 *   per-action approval.
 * - this historical supervised-mode scenario does not test the new product
 *   launcher or automatic scrolling; those tools are explicitly disabled here.
 *   Use browser:collaboration for native automatic-mode behavior, and manual
 *   product acceptance for the real application launcher.
 *
 * Failure modes this runner is built to surface (written before the checks):
 * - the plugin loads but the tools are unavailable, so a real tool call never
 *   runs (registration is not execution);
 * - the model reaches for a non-browser tool (file / shell / network);
 * - a shared page's fixture content reaching the model without the untrusted
 *   wrapper, or carrying account data instead of the isolated fixture marker;
 * - a mutation landing in the DOM before approval, more than once on approval,
 *   or with a synthesized (untrusted) input event;
 * - an interrupt that leaves the browser approval pending, or a cancelled call
 *   that still mutates;
 * - a manual take-over that fails to withdraw the grant, or a late approval
 *   acting after the grant is gone;
 * - the run inheriting the real user database / config / accounts, printing a
 *   key, or leaking one (or the daemon token) into an artifact;
 * - cleanup that leaks a process the runner did not own, or an unbounded run.
 *
 * This runner only ever runs when the automated-provider loop already proved the
 * plugin tool path, so it can spend its one resource (a real model call) on the
 * behavior the fake cannot see.
 */
import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { mkdir, readFile, readdir, rename, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, type Browser, type Page } from 'playwright-core';
import { startFixture, PAGE_IDS, type PageId } from './browser-poc-fixture.ts';
import { STEPFUN_MODEL, STEPFUN_PROVIDER, stepFunChildEnv } from './browser-tools-stepfun.ts';
import {
  DaemonSocket, fetchTranscriptText, waitNativeSession, wireStartOptions,
} from './fixtures/browser-tools-native/daemon-client.ts';
import { inspectAndRemoveIsolation } from './fixtures/browser-tools-native/isolation.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const noProxy = [process.env.NO_PROXY, process.env.no_proxy, '127.0.0.1', 'localhost', '::1'].filter(Boolean).join(',');
process.env.NO_PROXY = noProxy;
process.env.no_proxy = noProxy;

const EXAMPLE_TARGET_DIR = 'target/browser-bridge-e2e';
const POC_TARGET_DIR = 'target/browser-poc';
const BUILD_LIMIT_MS = 900_000;
/** Measured from the moment the builds finish; a real Step call may think for a
 *  while at low-effort, so each stage gets a generous but bounded window. */
const RUN_LIMIT_MS = 900_000;
const STAGE_TIMEOUT_MS = 180_000;
const OPENCODE_BINARY = process.env.FINTWIND_OPENCODE_BINARY ?? 'E:\\bun\\bin\\opencode.exe';
/** The provider/ids the user authorized for this native run. */
const MODEL_REF = `${STEPFUN_PROVIDER}/${STEPFUN_MODEL}`;
const VARIANT = process.env.FINTWIND_STEP5_VARIANT ?? 'low';
/** User DB the helper reads (read-only) for the single active StepFun key. */
const DEFAULT_CREDENTIAL_DB = 'C:\\Users\\潘雷\\.local\\share\\opencode\\opencode.db';
/** Where isolation lives — never in the repo, never under the report. */
const TEMP_BASE = process.env.FINTWIND_NATIVE_TMP ?? 'C:\\Users\\Public\\Temp\\opencode';

// Pull `--credential-db <path>` out before validating the remaining flags.
const rawArgs = process.argv.slice(2);
let credentialDb = process.env.FINTWIND_STEPFUN_CREDENTIAL_DB || DEFAULT_CREDENTIAL_DB;
const args = new Set<string>();
for (let index = 0; index < rawArgs.length; index += 1) {
  const arg = rawArgs[index]!;
  if (arg === '--credential-db') {
    const value = rawArgs[index + 1];
    if (!value || value.startsWith('--')) throw new Error('--credential-db requires a path');
    credentialDb = value;
    index += 1;
    continue;
  }
  args.add(arg);
}
for (const arg of args) {
  if (arg !== '--skip-build') throw new Error(`Unknown argument: ${arg}`);
}
assert(process.platform === 'win32', 'This E2E requires Windows and WebView2.');

// Distinctive, non-real values injected into the fixture; they must never
// appear in what the model receives after the native snapshot adapter redacts.
const PASSWORD_VALUE = 'native-not-a-real-secret';
const HIDDEN_VALUE = 'hidden-not-a-real-secret';
const COOKIE_VALUE = 'native-cookie-not-a-real-secret';

const runId = randomUUID();
const output = join(root, 'target', 'browser-tools-native', 'runs', runId);
const isoRoot = join(TEMP_BASE, `browser-tools-native-${runId}`);
const paths = {
  config: join(isoRoot, 'config'),
  data: join(isoRoot, 'data'),
  cache: join(isoRoot, 'cache'),
  stateHome: join(isoRoot, 'state'),
  tmp: join(isoRoot, 'tmp'),
  ocConfig: join(isoRoot, 'oc-config'),
  daemonState: join(isoRoot, 'daemon-state'),
  workspace: join(isoRoot, 'workspace'),
  opencodeDb: join(isoRoot, 'opencode.db'),
};
await mkdir(output, { recursive: true });
for (const dir of [paths.config, paths.data, paths.cache, paths.stateHome, paths.tmp, paths.ocConfig, paths.daemonState, paths.workspace]) {
  await mkdir(dir, { recursive: true });
}

const report = {
  runId,
  status: 'running' as 'running' | 'passed' | 'failed',
  startedAt: new Date().toISOString(),
  finishedAt: '',
  versions: {
    opencode: '',
    opencodePath: OPENCODE_BINARY,
    webview2: '',
    bun: Bun.version,
    protocol: 0,
    daemon: '',
    hostSha256: '',
    exampleSha256: '',
    pluginSourceSha256: '',
    model: MODEL_REF,
    modelVariant: VARIANT,
    realness: {
      model: `real StepFun ${STEPFUN_MODEL} (${VARIANT})`,
      daemonDriverBrokerRegistryPlugin: 'real',
      nativeHost: 'real WebView2 PoC testhost (not the product GUI)',
      providerPath: 'StepFun API via Fintwind private OpenCode server',
      credentialSource: 'isolated read-only StepFun row -> child env only',
    },
  },
  checks: [] as Array<{ name: string; status: 'passed' | 'failed' | 'skipped'; details: string; durationMs: number; realness: string }>,
  errors: [] as string[],
  startup: [] as Array<{ step: string; status: 'ok' | 'failed'; details: string }>,
  cleanup: [] as Array<{ step: string; status: 'ok' | 'failed'; details: string }>,
  artifacts: { report: join(output, 'report.json'), output },
};
await writeFile(report.artifacts.report, JSON.stringify(report, null, 2));

let daemon: ChildProcess | undefined;
let host: ChildProcess | undefined;
let browser: Browser | undefined;
let fixture: ReturnType<typeof startFixture> | undefined;
let client: DaemonSocket | undefined;
let readyLine = '';
let token = '';
let stepKey = '';
function redact(text: string): string {
  for (const secret of [stepKey, token]) if (secret) text = text.replaceAll(secret, '[REDACTED]');
  return text;
}
let daemonAddress = '';
let budgetStartedAt = 0;
let interrupted = false;
const interrupt = () => { interrupted = true; };
process.on('SIGINT', interrupt);
process.on('SIGTERM', interrupt);

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
  if (left <= 10_000) throw new Error('the run exhausted its time budget');
  return left;
}
async function deadline<T>(promise: Promise<T>, label: string, ms: number): Promise<T> {
  if (interrupted) throw new Error('E2E interrupted.');
  let timer: ReturnType<typeof setTimeout>;
  try {
    return await Promise.race([promise, new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error(`${label} timed out`)), ms); })]);
  } finally { clearTimeout(timer!); }
}
async function poll<T>(label: string, read: () => Promise<T> | T, accept: (value: T) => boolean, ms = 60_000): Promise<T> {
  const budget = Math.min(ms, await remainingMs());
  const until = Date.now() + budget;
  while (Date.now() < until) {
    if (interrupted) throw new Error('E2E interrupted.');
    let value: T;
    try { value = await deadline(Promise.resolve(read()), label, 3_000); } catch { await Bun.sleep(100); continue; }
    if (accept(value)) return value;
    await Bun.sleep(100);
  }
  throw new Error(`${label} timed out`);
}
async function check(name: string, realness: string, run: () => Promise<string | void>) {
  const start = performance.now();
  try {
    await remainingMs();
    const details = await deadline(run(), name, Math.max(10_000, await remainingMs()));
    report.checks.push({ name, status: 'passed', details: details ?? 'Verified against the live native host and daemon.', durationMs: Math.round(performance.now() - start), realness });
    console.log(`[OK] ${name}`);
  } catch (error) {
    const details = redact(error instanceof Error ? error.message : String(error));
    report.checks.push({ name, status: 'failed', details, durationMs: Math.round(performance.now() - start), realness });
    console.error(`[X] ${name}: ${details}`);
    try {
      await writeFile(join(output, `failure-${report.checks.length}.json`), JSON.stringify({
        host: await state().catch(() => null),
        nativeSessionId,
      }, null, 2));
    } catch { /* host may have exited; the message stands. */ }
    await writeFile(report.artifacts.report, JSON.stringify(report, null, 2));
    throw error;
  }
  await writeFile(report.artifacts.report, JSON.stringify(report, null, 2));
}
function record(list: 'startup' | 'cleanup', step: string, error: unknown) {
  const details = redact(error instanceof Error ? error.message : String(error));
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

// ---- Native host state + control (copied minimal from browser-collaboration). --
type Scope = { sessionId: string; runtimeId: string; pageId: string; grantId: string };
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
  pages: Array<{ id: PageId; ready: boolean; url: string | null; title: string | null; nativeFocusGains: number; error: string | null }>;
};
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
  await poll(`host ${action}`, requireState, value => value.lastControlId === requestId, 20_000);
  return requestId;
}

const sessionId = randomUUID();
const runtimeId = randomUUID();
let activeRuntimeId = runtimeId;
let origin = '';
let nativeSessionId = '';
let pages = {} as Partial<Record<PageId, Page>>;
let stageEventOffset = 0;
let pageMarker = '';
const focusGains = async () => (await requireState()).pages.map(page => page.nativeFocusGains);
const expectNoFocusChange = async (before: number[], what: string) => {
  assert.deepEqual(await focusGains(), before, `${what} moved the native keyboard away from its owner`);
};

/** Drive one deterministic prompt stage; returns without retrying. */
async function prompt(text: string) {
  stageEventOffset = client!.notifications.length;
  const outcome = await client!.request(sessionId, activeRuntimeId, { type: 'prompt', prompt: text }, randomUUID(), 30_000);
  assert(outcome.status === 'ok', `the prompt must be accepted: ${JSON.stringify(outcome)}`);
}

async function waitTurnFinished() {
  await poll('this real model turn finishes', () => client!.notifications.slice(stageEventOffset), rows => rows.some(row =>
    row.type === 'event' && row.sessionId === sessionId && row.runtimeId === activeRuntimeId
    && (row.event as { kind?: string } | undefined)?.kind === 'turnFinished'), STAGE_TIMEOUT_MS);
}

try {
  report.versions.opencode = await (async () => {
    const c = spawn(OPENCODE_BINARY, ['--version'], { shell: false, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, NO_PROXY: noProxy, no_proxy: noProxy } });
    let out = ''; c.stdout!.on('data', b => { out += String(b); }); c.stderr!.on('data', b => { out += String(b); });
    assert.equal(await deadline(exit(c), 'opencode --version', 30_000), 0);
    return out.split(/\r?\n/).map(l => l.trim()).find(l => l.length > 0) ?? 'unknown';
  })();
  report.startup.push({ step: 'opencode version', status: 'ok', details: `${report.versions.opencode} at ${OPENCODE_BINARY}` });

  // Build the daemon example and require the native host binary.
  if (!args.has('--skip-build')) {
    const build = spawn('cargo', ['build', '--locked', '--package', 'fintwind-core', '--example', 'browser_tools_opencode', '--target-dir', EXAMPLE_TARGET_DIR], { cwd: root, shell: false, stdio: 'inherit', windowsHide: true });
    try { assert.equal(await deadline(exit(build), 'example build', BUILD_LIMIT_MS), 0); } finally { if (build.exitCode === null) build.kill(); }
  }
  const exampleExe = join(root, EXAMPLE_TARGET_DIR, 'debug', 'examples', 'browser_tools_opencode.exe');
  const hostExe = join(root, POC_TARGET_DIR, 'debug', 'fintwind.exe');
  await readFile(exampleExe);
  await readFile(hostExe);
  await readFile(OPENCODE_BINARY);
  report.versions.exampleSha256 = createHash('sha256').update(await readFile(exampleExe)).digest('hex');
  report.versions.hostSha256 = createHash('sha256').update(await readFile(hostExe)).digest('hex');
  const pluginPath = join(root, 'resources', 'opencode-browser-plugin.ts');
  report.versions.pluginSourceSha256 = createHash('sha256').update(await readFile(pluginPath)).digest('hex');
  budgetStartedAt = Date.now();

  // ---- Isolation + the authorized StepFun credential (child env only). --------
  const tools = {
    read: false, write: false, edit: false, multiedit: false, patch: false,
    glob: false, grep: false, webfetch: false, websearch: false,
    shell: false, bash: false, skill: false, subagent: false, task: false, question: false, execute: false,
    'fintwind_browser_list': true, 'fintwind_browser_snapshot': true,
    'fintwind_browser_click': true, 'fintwind_browser_fill': true, 'fintwind_browser_navigate': true,
    'fintwind_browser_open': false, 'fintwind_browser_scroll': false,
  };
  const opencodeConfigContent = JSON.stringify({
    $schema: 'https://opencode.ai/config.json',
    small_model: MODEL_REF,
    tools,
  });
  const isolatedBase: NodeJS.ProcessEnv = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (key.toUpperCase().startsWith('WEBVIEW2_')) continue;
    if (key.startsWith('OPENCODE_')) continue;                       // no inherited OpenCode config/creds
    if (key === 'FINTWIND_BROWSER_TOOL_ADDRESS' || key === 'FINTWIND_BROWSER_TOOL_TOKEN') continue;
    isolatedBase[key] = value;
  }
  Object.assign(isolatedBase, {
    FINTWIND_BROWSER_TOOLS_E2E_STATE: paths.daemonState,
    OPENCODE_CONFIG_CONTENT: opencodeConfigContent,
    OPENCODE_CONFIG_DIR: paths.ocConfig,
    XDG_CONFIG_HOME: paths.config,
    XDG_DATA_HOME: paths.data,
    XDG_CACHE_HOME: paths.cache,
    XDG_STATE_HOME: paths.stateHome,
    OPENCODE_DISABLE_AUTOUPDATE: '1',
    OPENCODE_DISABLE_FILEWATCHER: '1',
    OPENCODE_DISABLE_PROJECT_CONFIG: '1',
    OPENCODE_CONFIG_PROJECT_DISABLE: '1',
    TMPDIR: paths.tmp,
    OPENCODE_DB: paths.opencodeDb,
    NO_PROXY: noProxy,
    no_proxy: noProxy,
  });
  // stepFunChildEnv strips every other SDK key and injects only the active
  // StepFun key, read-only from the credential database, into the child env.
  const childEnv = stepFunChildEnv(isolatedBase, credentialDb);
  stepKey = childEnv.STEPFUN_API_KEY ?? '';
  report.startup.push({ step: 'credential hand-off', status: 'ok', details: `StepFun key injected into one child env from a read-only credential database; model ${MODEL_REF} (${VARIANT}); runner writes no key to config; SDK isolation is scanned and removed after shutdown` });

  // ---- The real daemon (FintwindBackend + serve). ----------------------------
  daemon = spawn(exampleExe, [], { cwd: root, shell: false, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: childEnv });
  daemon.stdout!.on('data', bytes => { readyLine += String(bytes); });
  daemon.stderr!.on('data', bytes => { process.stderr.write(`[daemon] ${redact(String(bytes))}`); });
  if (daemon.exitCode !== null) throw new Error(`the daemon exited during startup: ${daemon.exitCode}`);
  await poll('daemon ready line', () => readyLine.includes('browser-tools-opencode') && readyLine.includes('\n'), ok => ok, 30_000);
  let ready: { kind: string; address: string; token: string; protocolVersion: number } | undefined;
  try { ready = JSON.parse(readyLine.split('\n').find(line => line.includes('browser-tools-opencode'))!); }
  catch { throw new Error('The daemon did not return a valid ready line.'); }
  assert(ready && ready.address.startsWith('127.0.0.1:'), 'the daemon listener is loopback only');
  token = ready.token;
  report.versions.protocol = ready.protocolVersion;
  daemonAddress = `ws://${ready.address}/v1`;

  client = new DaemonSocket(daemonAddress, token);
  client.protocolVersion = ready.protocolVersion;
  await client.connect();
  report.versions.daemon = client.hello?.daemonVersion ?? '';
  report.startup.push({ step: 'daemon', status: 'ok', details: `${daemonAddress} (version ${report.versions.daemon})` });

  // ---- Start the real Step runtime (this is the first real model surface). -----
  await check(`the private Step runtime starts with ${MODEL_REF} (${VARIANT})`, 'real private OpenCode + real driver; tool execution not yet proven', async () => {
    const outcome = await client!.request(sessionId, runtimeId, { type: 'start', options: wireStartOptions({ binary: OPENCODE_BINARY, cwd: paths.workspace, model: MODEL_REF, reasoningEffort: VARIANT }) }, randomUUID(), 120_000);
    assert(outcome.status === 'ok', `the Step runtime must start: ${JSON.stringify(outcome)}`);
    nativeSessionId = await waitNativeSession(client!, 30_000);
    report.startup.push({ step: 'session', status: 'ok', details: nativeSessionId });
    return `Private OpenCode session ${nativeSessionId.slice(0, 8)} started with ${MODEL_REF} (${VARIANT}); a successful tool round-trip must separately prove browser activation.`;
  });

  // ---- The real WebView2 PoC host. --------------------------------------------
  fixture = startFixture(runId);
  origin = fixture.origin;
  const cdpPort = await unusedPort();
  const hostEnv = { ...childEnv };
  delete hostEnv.STEPFUN_API_KEY;
  host = spawn(hostExe, [
    '--browser-poc',
    `--cdp-port=${cdpPort}`,
    `--fixture-origin=${origin}`,
    `--artifact-dir=${output}`,
    `--run-id=${runId}`,
    `--bridge-address=ws://${ready.address}`,
    `--bridge-token=${token}`,
    `--bridge-session=${sessionId}`,
    `--bridge-runtime=${runtimeId}`,
  ], { cwd: root, env: hostEnv, shell: false, windowsHide: false, stdio: ['ignore', 'pipe', 'pipe'] });
  host.stdout!.pipe(createWriteStream(join(output, 'host.stdout.log')));
  host.stderr!.pipe(createWriteStream(join(output, 'host.stderr.log')));
  host.on('error', error => { report.errors.push(error.message); interrupted = true; });
  const initial = await poll('host ready with a live bridge', state, value => {
    if (!value) return false;
    assert.equal(value.runId, runId);
    const failed = value.pages.filter(page => page.error);
    assert.equal(failed.length, 0, JSON.stringify(failed));
    return value.pages.length === 3
      && value.pages.every(page => page.ready && page.url?.includes(runId))
      && value.bridge?.configured === true
      && value.bridge?.connected === true
      && value.browserPages.length === 3;
  }, 90_000);
  assert(initial && initial.bridge?.sessionId === sessionId && initial.bridge?.runtimeId === runtimeId, 'the host shares this daemon session and runtime');
  report.startup.push({ step: 'native host', status: 'ok', details: `pid ${host.pid}, 3 pages, bridge connected` });

  browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`, { noDefaults: true, isLocal: true, timeout: 10_000 });
  report.versions.webview2 = browser.version();
  report.startup.push({ step: 'webview2', status: 'ok', details: report.versions.webview2 });
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
  // Setup only: values the native snapshot adapter must redact before the model
  // sees them. Playwright injects; it performs none of the actions under test.
  await pages.alpha!.evaluate(({ passwordValue, hiddenValue, cookieValue }) => {
    const password = document.createElement('input');
    password.id = 'e2e-password'; password.type = 'password'; password.value = passwordValue;
    document.body.append(password);
    const hidden = document.createElement('input');
    hidden.id = 'e2e-hidden'; hidden.type = 'hidden'; hidden.value = hiddenValue;
    document.body.append(hidden);
    document.cookie = `e2e-cookie=${cookieValue}; path=/`;
  }, { passwordValue: PASSWORD_VALUE, hiddenValue: HIDDEN_VALUE, cookieValue: COOKIE_VALUE });
  pageMarker = 'native-page-' + randomUUID();
  await pages.alpha!.evaluate(marker => {
    const paragraph = document.createElement('p');
    paragraph.textContent = marker;
    document.body.prepend(paragraph);
  }, pageMarker);

  // Share exactly one page (alpha) with the real runtime.
  await check('the native host shares exactly alpha for the Step runtime', 'real native host -> real broker', async () => {
    const grantId = randomUUID();
    const focusBefore = await focusGains();
    await control('share-page', { pageId: 'alpha', grantId, runtimeId: activeRuntimeId });
    await poll('alpha shared', requireState, value => value.browserShares.length === 1
      && value.browserShares[0]?.page === 'alpha'
      && value.browserShares[0]?.scope.sessionId === sessionId
      && value.browserShares[0]?.scope.runtimeId === activeRuntimeId, 15_000);
    await poll('the broker accepted alpha', async () => {
      const response = await client!.request(sessionId, activeRuntimeId, { type: 'browserList' });
      return (response.payload as { value?: Array<{ scope: Scope }> } | undefined)?.value;
    }, shared => !!shared?.some(page => page.scope.grantId === grantId), 15_000);
    await expectNoFocusChange(focusBefore, 'sharing a page');
    return `one lease, one page, exact scope ${grantId.slice(0, 8)}.`;
  });

  const fixtureCount = (id: PageId) => pages[id]!.locator('#count-value').textContent();

  // ---------------------------------------------------------------------------
  // Stage 1 — the model really lists and snapshots, and sees the fixture marker
  // (and no account data) through the untrusted wrapper.
  // ---------------------------------------------------------------------------
  await check('the Step model really lists and snapshots, seeing the isolated fixture (no accounts, passwords redacted)', 'real Model', async () => {
    await prompt('Use your Fintwind browser tools. Call fintwind_browser_list. Then call fintwind_browser_snapshot on the first shared page and read its visible text. Reply with only the page title you saw. If fintwind_browser_list is unavailable, reply exactly UNAVAILABLE and stop. Do not use any other tool.');
    const marker = pageMarker;
    const text = await poll('the snapshot reached the model (transcript shows the shared page)', async () => {
      const t = await fetchTranscriptText(client!, { binary: OPENCODE_BINARY, cwd: paths.workspace, sessionId: nativeSessionId });
      return t.includes(marker) ? t : '';
    }, ok => ok.length > 0, STAGE_TIMEOUT_MS);
    assert(!text.includes('UNAVAILABLE'), 'the model did not report the tools as unavailable');
    assert(text.includes('untrusted_shared_browser_page'), 'the snapshot reached the model wrapped as an untrusted source');
    assert(text.includes(stepMarker()), 'the model saw the shared fixture page, not an account page');
    assert(!text.includes(PASSWORD_VALUE) && !text.includes(HIDDEN_VALUE) && !text.includes(COOKIE_VALUE),
      'the native snapshot adapter redacted password, hidden and cookie values before the model saw them');
    await waitTurnFinished();
    return `list + snapshot reached the model over the real tool path; the untrusted wrapper carried the fixture marker (${marker}) and no secrets.`;
  });

  // ---------------------------------------------------------------------------
  // Stage 2 — a click is gated: nothing before approval, exactly one after.
  // ---------------------------------------------------------------------------
  await check('a requested click is gated: held (count 0), then exactly one on approval, trusted input', 'real Model -> real native host', async () => {
    await prompt('Call fintwind_browser_list, then call fintwind_browser_click on the first shared page to click its #count button exactly once. Do not click anything else.');
    const pending = await poll('a click is pending on alpha', requireState, value =>
      value.pendingBrowserRequests.some(r => r.page === 'alpha' && r.action === 'click' && r.detail.includes('count')), STAGE_TIMEOUT_MS);
    const request = pending.pendingBrowserRequests.find(r => r.page === 'alpha' && r.action === 'click')!;
    assert.equal(await fixtureCount('alpha'), '0', 'no click before approval');
    assert.equal(await pages.gamma!.locator('#count-value').textContent(), '0', 'gamma untouched');
    const focusBefore = await focusGains();
    await control('approve-browser', { browserRequestId: request.requestId });
    await poll('alpha counted once', () => fixtureCount('alpha'), v => v === '1', 15_000);
    const trusted = await pages.alpha!.evaluate(() => (window as unknown as { pocEvents: { clickTrusted: boolean } }).pocEvents);
    assert.equal(trusted.clickTrusted, true, 'the approved click must be trusted native input');
    assert.equal(await pages.beta!.locator('#count-value').textContent(), '0', 'beta untouched');
    assert.equal(await pages.gamma!.locator('#count-value').textContent(), '0', 'gamma untouched');
    await expectNoFocusChange(focusBefore, 'the approved click');
    // The model saw the click settle as a success tool result.
    await poll('the model received the click success', () => fetchTranscriptText(client!, { binary: OPENCODE_BINARY, cwd: paths.workspace, sessionId: nativeSessionId }), t => t.includes('fintwind_browser_click'), STAGE_TIMEOUT_MS);
    await waitTurnFinished();
    assert.equal(await fixtureCount('alpha'), '1', 'the settled model turn applied only one click');
    return 'held click applied nothing; after approval exactly one trusted click landed on alpha only, and the model received the success.';
  });

  // ---------------------------------------------------------------------------
  // Stage 3 — a fill is gated the same way, trusted, and only its own text.
  // ---------------------------------------------------------------------------
  await check('a requested fill is gated: held (empty), then exactly its own text, trusted input', 'real Model -> real native host', async () => {
    const marker = `native-e2e-${runId.slice(0, 8)}`;
    await prompt('Call fintwind_browser_list, then call fintwind_browser_fill on the first shared page to set its #name input to exactly this text: ' + marker + '. Fill nothing else.');
    const pending = await poll('a fill is pending on alpha', requireState, value =>
      value.pendingBrowserRequests.some(r => r.page === 'alpha' && r.action === 'fill'), STAGE_TIMEOUT_MS);
    const request = pending.pendingBrowserRequests.find(r => r.page === 'alpha' && r.action === 'fill')!;
    assert.equal(await pages.alpha!.locator('#name').inputValue(), '', 'no text typed before approval');
    await control('approve-browser', { browserRequestId: request.requestId });
    await poll('alpha filled', () => pages.alpha!.locator('#name-value').textContent(), v => v === marker, 15_000);
    const trusted = await pages.alpha!.evaluate(() => (window as unknown as { pocEvents: { inputTrusted: boolean } }).pocEvents);
    assert.equal(trusted.inputTrusted, true, 'the fill must be trusted native input');
    assert.equal(await pages.alpha!.locator('#name').inputValue(), marker, 'only the fill\'s own text landed');
    await waitTurnFinished();
    assert.equal(await pages.alpha!.locator('#name').inputValue(), marker, 'the settled fill did not overwrite its own value');
    return `fill held until approval, then typed exactly "${marker}" through the native path.`;
  });

  // ---------------------------------------------------------------------------
  // Stage 4 — Cancel becomes an OpenCode interrupt that withdraws the pending
  // browser work and changes nothing.
  // ---------------------------------------------------------------------------
  await check('a daemon Cancel interrupts the OpenCode turn and drops the pending click (no DOM change)', 'real driver + real OpenCode interrupt', async () => {
    await prompt('Call fintwind_browser_list, then call fintwind_browser_click on the first shared page to click its #count button once.');
    const pending = await poll('a click is pending on alpha', requireState, value =>
      value.pendingBrowserRequests.some(r => r.page === 'alpha' && r.action === 'click'), STAGE_TIMEOUT_MS);
    const before = await fixtureCount('alpha');
    const cancelled = client!.request(sessionId, activeRuntimeId, { type: 'cancel' }, randomUUID(), 20_000);
    await poll('the broker told the host to abandon the approval', requireState, value =>
      !value.pendingBrowserRequests.some(r => r.page === 'alpha' && r.action === 'click'), 25_000);
    assert.equal((await cancelled).status, 'ok', 'the driver accepted the real interrupt');
    await waitTurnFinished();
    assert.equal(await fixtureCount('alpha'), before, 'a cancelled click did not run');
    return 'Command::Cancel turned into /api/session/{id}/interrupt; context.signal aborted, the plugin cancelled, and the host dropped the approval with no DOM change.';
  });

  // ---------------------------------------------------------------------------
  // Stage 5 — a manual take-over (host revoke, same guard) withdraws the grant;
  // the in-flight call fails and a late approval acts on nothing.
  // ---------------------------------------------------------------------------
  await check('a manual take-over revoke withdraws the grant, fails the call, and a late approval acts on nothing', 'real native host guard', async () => {
    await prompt('Call fintwind_browser_list, then call fintwind_browser_click on the first shared page to click its #count button once.');
    const pending = await poll('a click is pending on alpha', requireState, value =>
      value.pendingBrowserRequests.some(r => r.page === 'alpha' && r.action === 'click'), STAGE_TIMEOUT_MS);
    const request = pending.pendingBrowserRequests.find(r => r.page === 'alpha' && r.action === 'click')!;
    const before = await fixtureCount('alpha');
    // Human take-over: the page owner (the host's guard) revokes the share.
    await control('revoke-page', { pageId: 'alpha', runtimeId: activeRuntimeId });
    await poll('the revoked call failed and the grant is gone', requireState, value =>
      value.browserShares.length === 0 && !value.pendingBrowserRequests.some(r => r.page === 'alpha'), 25_000);
    assert.equal(await fixtureCount('alpha'), before, 'a revoked grant cannot mutate');
    // A late approval for the dead request must not resurrect it.
    await control('approve-browser', { browserRequestId: request.requestId });
    await Bun.sleep(700);
    assert.equal(await fixtureCount('alpha'), before, 'a late approval after revoke must not run');
    assert.equal((await requireState()).browserShares.length, 0, 'nothing stays shared');
    await waitTurnFinished();
    return 'manual take-over revoked the grant, the in-flight click failed, and a late approval did nothing.';
  });

  report.startup.push({ step: 'checks complete', status: 'ok', details: `${report.checks.filter(c => c.status === 'passed').length} passed` });
} catch (error) {
  const details = redact(error instanceof Error ? error.stack ?? error.message : String(error));
  report.errors.push(details);
  console.error('[X] E2E could not complete:', details);
} finally {
  await Promise.allSettled(inflight.map(entry => entry.promise));
  // Reap the native host first (its daemon-connection drop revokes its own
  // leases), then the daemon (whose shutdown_all reaps `opencode serve`).
  if (host && host.exitCode === null && host.signalCode === null) {
    try {
      const exited = exit(host);
      await control('shutdown', {}).catch(() => { /* If control failed the kill below covers it. */ });
      await deadline(exited, 'host shutdown', 8_000);
      report.cleanup.push({ step: 'host shutdown', status: 'ok', details: `pid ${host.pid}` });
    } catch (error) {
      if (host.pid && host.exitCode === null && host.signalCode === null) {
        const kill = spawn('taskkill', ['/PID', String(host.pid), '/T', '/F'], { shell: false, windowsHide: true, stdio: 'ignore' });
        try {
          assert.equal(await deadline(exit(kill), 'host taskkill', 8_000), 0);
          report.cleanup.push({ step: 'host taskkill', status: 'ok', details: `pid ${host.pid} (tree)` });
        } catch (killError) { record('cleanup', 'host taskkill', killError); }
      } else { record('cleanup', 'host shutdown', error); }
    }
  }
  if (daemon && daemon.exitCode === null && daemon.signalCode === null) {
    try {
      const shutdown = new DaemonSocket(daemonAddress, token);
      shutdown.protocolVersion = report.versions.protocol;
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
  try { client?.close(); } catch { /* ignore */ }
  if (browser) { try { await deadline(browser.close(), 'playwright disconnect', 5_000); } catch (error) { record('cleanup', 'playwright disconnect', error); } }
  if (fixture) { try { fixture.stop(); report.cleanup.push({ step: 'fixture stop', status: 'ok', details: 'both loopback servers stopped' }); } catch (error) { record('cleanup', 'fixture stop', error); } }
  if (!report.cleanup.some(row => row.status === 'failed' && /host|daemon/.test(row.step))) {
    try {
      const scanned = await inspectAndRemoveIsolation(isoRoot, runId, [stepKey, token]);
      report.cleanup.push({ step: 'isolation credential scan and removal', status: 'ok',
        details: `${scanned.files} files / ${scanned.bytes} bytes scanned including DB/WAL; ${scanned.matchedFiles} matching files; owned isolation removed` });
      if (scanned.matchedFiles) report.errors.push('A credential was persisted inside SDK isolation; isolation was removed.');
    } catch (error) { record('cleanup', 'isolation credential scan and removal', error); }
  } else {
    record('cleanup', 'isolation removal', new Error('Isolation retained because an owned process could not be stopped.'));
  }
  if (interrupted && !report.errors.includes('E2E interrupted.')) report.errors.push('E2E interrupted.');

  report.finishedAt = new Date().toISOString();
  report.status = report.errors.length || report.checks.some(c => c.status === 'failed') || report.cleanup.some(s => s.status === 'failed') ? 'failed' : 'passed';
  await writeFile(report.artifacts.report, JSON.stringify(report, null, 2));
  const secretScan = await scanForSecrets(output, token);
  if (secretScan) report.errors.push(secretScan);
  report.status = report.errors.length ? 'failed' : report.status;
  await writeFile(report.artifacts.report, JSON.stringify(report, null, 2));
  console.log(`Report: ${report.artifacts.report}`);
  process.exitCode = report.status === 'passed' ? 0 : 1;
}

/** The unique fixture marker the shared page carries for this run. */
function stepMarker(): string {
  return `run=${runId}`;
}

/** Scan only this run's small text artifacts for the daemon token or a stray
 *  key; databases and the WebView2 profile are skipped, not read. */
async function scanForSecrets(dir: string, daemonToken: string): Promise<string | null> {
  let entries: string[] = [];
  try { entries = await readdir(dir); } catch { return null; }
  for (const entry of entries) {
    if (!/\.(json|log|out|txt)$/i.test(entry)) continue; // skip DB / profile / binaries
    try {
      const text = await readFile(join(dir, entry), 'utf8');
      if (daemonToken && text.includes(daemonToken)) return `an artifact leaked the daemon token: ${entry}`;
      if (stepKey && text.includes(stepKey)) return `an artifact leaked the selected StepFun credential: ${entry}`;
      if (/["']?(apiKey|STEPFUN_API_KEY)["']?\s*[:=]\s*["']?[A-Za-z0-9_\-]{20,}/.test(text)) return `an artifact appears to carry an API key: ${entry}`;
    } catch { return `an artifact could not be checked for credentials: ${entry}`; }
  }
  return null;
}
