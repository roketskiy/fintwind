/**
 * Behavioral E2E, not a visual or change-detector test. Failure modes:
 * wrong browser/profile/tab; untrusted inputs; broken waiting/frames;
 * stale host URL/title; stolen native focus; reconnect destroys pages;
 * host close leaves orphan targets; timeout/cleanup loses evidence.
 */
import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { mkdir, readFile, rename, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, type Browser, type Page } from 'playwright-core';
import { startFixture } from './browser-poc-fixture.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ids = ['alpha', 'beta', 'gamma'] as const;
type PageId = typeof ids[number];
type HostState = {
  runId: string;
  profile: string;
  pid: number;
  gpuiFocused: boolean;
  lastControlId: string | null;
  pages: Array<{
    id: PageId; url: string | null; title: string | null;
    ready: boolean; loading: boolean; error: string | null; nativeFocused: boolean;
    nativeFocusGains: number; nativeVisible: boolean; nativeBounds: number[] | null;
  }>;
};
type Check = { name: string; status: 'passed' | 'failed' | 'skipped'; details: string; durationMs: number };

const args = new Set(process.argv.slice(2));
for (const arg of args) {
  if (arg !== '--skip-build' && arg !== '--screenshots') throw new Error(`Unknown argument: ${arg}`);
}
if (process.platform !== 'win32') throw new Error('This probe requires Windows and WebView2.');
const runId = randomUUID();
const output = join(root, 'target', 'browser-poc', 'runs', runId);
await mkdir(output, { recursive: true });
const report = {
  runId, status: 'running', startedAt: new Date().toISOString(), finishedAt: '',
  versions: { bun: Bun.version, playwright: '1.63.0', browser: '', hostSha256: '' },
  checks: [] as Check[], errors: [] as string[],
  limitations: [
    'Raw local CDP is not an authorization boundary. Only a new test profile is exposed.',
    'No OpenCode agent, production permissions, manual takeover, or remote daemon is tested.',
    'External Target.createTarget and page.close are not enabled; page lifetime belongs to the host.',
    'No visual comparison or streaming-performance claim is made.',
    'High-level input is verified on visible native surfaces, not minimized windows or hidden tabs.',
    'An OS-selected port is released before WebView2 binds it; contention fails instead of attaching blindly.',
  ],
};
// Preserve an explicitly incomplete artifact even if the runner is forcibly
// terminated before it can enter its finally block.
await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2));
let host: ChildProcess | undefined;
let browser: Browser | undefined;
let fixture: ReturnType<typeof startFixture> | undefined;
let endpoint: string | undefined;
let interrupted = false;
const interrupt = () => { interrupted = true; };
process.on('SIGINT', interrupt);
process.on('SIGTERM', interrupt);
const alive = () => {
  if (interrupted) throw new Error('Probe interrupted.');
  if (host && (host.exitCode !== null || host.signalCode !== null)) {
    throw new Error(`Probe host exited: ${host.exitCode ?? host.signalCode}`);
  }
};
async function deadline<T>(promise: Promise<T>, label: string, ms = 15_000): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error(`${label} timed out`)), ms); }),
    ]);
  } finally { clearTimeout(timer!); }
}
async function poll<T>(label: string, read: () => Promise<T>, accept: (value: T) => boolean, ms = 15_000): Promise<T> {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    alive();
    const value = await deadline(read(), label, 3000);
    if (accept(value)) return value;
    await Bun.sleep(100);
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
async function control(action: 'focus-gpui' | 'close-page' | 'shutdown', pageId?: PageId) {
  const requestId = randomUUID();
  const path = join(output, 'control.json');
  await writeFile(`${path}.tmp`, JSON.stringify({ requestId, action, pageId }));
  await rename(`${path}.tmp`, path);
  if (action !== 'shutdown') {
    await poll(`Host ${action}`, requireState, value => value.lastControlId === requestId);
  }
}
async function check(name: string, run: () => Promise<string | void>) {
  const start = performance.now();
  try {
    alive();
    const details = await deadline(run(), name, 40_000);
    report.checks.push({ name, status: 'passed', details: details ?? 'Verified against the live WebView2 host.', durationMs: Math.round(performance.now() - start) });
    console.log(`[OK] ${name}`);
  } catch (error) {
    const details = error instanceof Error ? error.message : String(error);
    report.checks.push({ name, status: 'failed', details, durationMs: Math.round(performance.now() - start) });
    console.error(`[X] ${name}: ${details}`);
    if (browser) {
      try {
        const diagnostics = await deadline(Promise.all(browser.contexts().flatMap(context => context.pages()).map(async page => ({
          url: page.url(),
          state: await page.evaluate('({ visibility: document.visibilityState, width: innerWidth, height: innerHeight, ready: document.readyState, count: document.querySelector("#count")?.getBoundingClientRect().toJSON() })'),
        }))), 'Failure diagnostics', 3000);
        await writeFile(join(output, `failure-${report.checks.length}.json`), JSON.stringify({ diagnostics, host: await state() }, null, 2));
      } catch { /* Retain the original failure even if the browser cannot answer. */ }
    }
  }
  await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2));
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
function exit(child: ChildProcess): Promise<number | null> {
  return new Promise((ok, fail) => {
    child.once('error', fail);
    child.once('exit', ok);
  });
}

try {
  if (!args.has('--skip-build')) {
    const build = spawn('cargo', ['build', '--locked', '--package', 'fintwind', '--features', 'browser-poc', '--bin', 'fintwind', '--target-dir', 'target/browser-poc'], { cwd: root, shell: false, stdio: 'inherit' });
    try { assert.equal(await deadline(exit(build), 'Isolated build', 1_200_000), 0); }
    finally { if (build.exitCode === null) build.kill(); }
  }
  const executable = join(root, 'target', 'browser-poc', 'debug', 'fintwind.exe');
  report.versions.hostSha256 = createHash('sha256').update(await readFile(executable)).digest('hex');
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.toUpperCase().startsWith('WEBVIEW2_')));
  await check('invalid configuration and inherited profile overrides fail closed', async () => {
    const cases = [
      { args: ['--cdp-port=0'], env, message: 'nonzero' },
      { args: ['--cdp-port=12345', '--cdp-port=12346'], env, message: 'duplicate' },
      { args: ['--fixture-origin=https://example.com'], env, message: 'must be http' },
      { args: [], env: { ...env, WEBVIEW2_USER_DATA_FOLDER: output }, message: 'refuses to start' },
    ];
    for (const scenario of cases) {
      const child = spawn(executable, ['--browser-poc', ...scenario.args], { cwd: root, env: scenario.env, shell: false, windowsHide: true, stdio: ['ignore', 'ignore', 'pipe'] });
      let stderr = '';
      child.stderr!.on('data', bytes => { stderr += String(bytes); });
      try {
        assert.equal(await deadline(exit(child), 'Rejected configuration', 5000), 1);
        assert(stderr.includes(scenario.message), stderr);
      } finally { if (child.exitCode === null && child.signalCode === null) child.kill(); }
    }
    await assert.rejects(stat(join(output, 'profile')), { code: 'ENOENT' });
    return 'Rejected invalid flags and a synthetic WEBVIEW2_USER_DATA_FOLDER before creating a profile or opening pages.';
  });
  fixture = startFixture(runId);
  const origin = fixture.origin;
  const port = await unusedPort();
  endpoint = `http://127.0.0.1:${port}`;
  let existing = false;
  try { existing = (await fetch(`${endpoint}/json/version`, { signal: AbortSignal.timeout(500) })).ok; } catch { /* Expected: no listener. */ }
  assert(!existing, 'The selected CDP port must not already be in use');
  // windowsHide sets a startup visibility hint for this process's GUI too.
  // A hidden GPUI window receives no layout frames for late controllers.
  host = spawn(executable, ['--browser-poc', `--cdp-port=${port}`, `--fixture-origin=${origin}`, `--artifact-dir=${output}`, `--run-id=${runId}`], { cwd: root, env, shell: false, windowsHide: false, stdio: ['ignore', 'pipe', 'pipe'] });
  host.stdout!.pipe(createWriteStream(join(output, 'host.stdout.log')));
  host.stderr!.pipe(createWriteStream(join(output, 'host.stderr.log')));
  host.on('error', error => { report.errors.push(error.message); interrupted = true; });
  const initial = await poll('Three initialized native pages', state, value => {
    if (!value) return false;
    const errors = value.pages.filter(page => page.error);
    assert.equal(errors.length, 0, JSON.stringify(errors));
    return value.pages.length === 3 && value.pages.every(page => page.ready && !page.loading && page.url?.includes(runId));
  }, 30_000);
  assert(initial);
  assert.equal(initial.runId, runId);
  assert.equal(initial.pid, host.pid);
  const actualProfile = initial.profile.startsWith('\\\\?\\') ? initial.profile.slice(4) : initial.profile;
  assert.equal(resolve(actualProfile).toLowerCase(), join(output, 'profile').toLowerCase());
  browser = await chromium.connectOverCDP(endpoint, { noDefaults: true, isLocal: true, timeout: 10_000 });
  report.versions.browser = browser.version();
  await writeFile(join(output, 'cdp-version.json'), JSON.stringify({ browser: report.versions.browser, playwright: report.versions.playwright, endpoint }, null, 2));
  const context = browser.contexts()[0];
  assert(context, 'WebView2 exposes a default context');
  context.setDefaultTimeout(8000);
  context.setDefaultNavigationTimeout(10_000);
  const pages = {} as Record<PageId, Page>;
  for (const id of ids) {
    const candidates: Page[] = context.pages().filter(page => {
      const url = new URL(page.url());
      return url.origin === origin && url.pathname === `/page/${id}` && url.searchParams.get('run') === runId;
    });
    assert.equal(candidates.length, 1, `Exactly one owned page for ${id}`);
    pages[id] = candidates[0]!;
  }
  assert.equal(context.pages().length, 3, 'No unowned targets in the isolated profile');
  const alpha = pages.alpha;

  await check('same-page identity and structured snapshot', async () => {
    const snapshot = await alpha.locator('body').ariaSnapshot();
    assert(/button "count"/i.test(snapshot));
    await writeFile(join(output, 'alpha.aria.txt'), snapshot);
    for (const id of ids) assert.equal(await pages[id].locator('body').getAttribute('data-page-id'), id);
    return 'Three live host pages mapped by run ID and page ID; ARIA snapshot retained.';
  });
  await check('fresh profile and shared test cookie', async () => {
    assert.equal((await context.cookies(origin)).length, 0, 'No previous-run cookies');
    await context.addCookies([{ name: 'poc-session', value: runId, url: origin }]);
    for (const id of ids) assert.equal(await pages[id].evaluate("document.cookie.includes('poc-session=' + " + JSON.stringify(runId) + ')'), true);
    return 'A synthetic cookie is shared only between the three test pages; values are not exported.';
  });
  await check('trusted click/input and no cross-tab mutation', async () => {
    await alpha.locator('#count').click();
    await alpha.locator('#name').fill('Fintwind probe');
    assert.equal(await alpha.locator('#count-value').textContent(), '1');
    assert.equal(await alpha.locator('#name-value').textContent(), 'Fintwind probe');
    const trusted = await alpha.evaluate<{ clickTrusted: boolean; inputTrusted: boolean }>('window.pocEvents');
    assert.equal(trusted.clickTrusted, true);
    assert.equal(trusted.inputTrusted, true);
    for (const id of ['beta', 'gamma'] as const) assert.equal(await pages[id].locator('#count-value').textContent(), '0');
  });
  await check('all three visible native surfaces accept input', async () => {
    await poll('All native surfaces laid out and visible', requireState, value => value.pages.every(page => page.nativeVisible && page.nativeBounds && page.nativeBounds[2]! > 0 && page.nativeBounds[3]! > 0), 5000);
    for (const id of ['beta', 'gamma'] as const) {
      await pages[id].locator('#count').click();
      assert.equal(await pages[id].locator('#count-value').textContent(), '1');
    }
  });
  await check('auto-wait for delayed and occluded buttons', async () => {
    await alpha.locator('#show-delayed').click();
    await alpha.locator('#delayed').click();
    assert.equal(await alpha.locator('#delayed-result').textContent(), 'clicked');
    await alpha.locator('#show-blocked').click();
    await alpha.locator('#blocked').click();
    assert.equal(await alpha.locator('#blocked-result').textContent(), 'clicked');
  });
  await check('cross-origin iframe input', async () => {
    const frame = alpha.frameLocator('iframe[title="Cross-origin fixture"]');
    await frame.locator('#frame-count').click();
    assert.equal(await frame.locator('#frame-value').textContent(), '1');
  });
  await check('host URL/title follows same-document navigation', async () => {
    await alpha.locator('#route').click();
    assert(new URL(alpha.url()).hash === '#routed');
    await poll('Host route state', requireState, value => value.pages.some(page => page.id === 'alpha' && page.url === alpha.url() && page.title?.endsWith(' Route')));
  });
  for (const operation of ['read', 'navigate', 'click', 'fill'] as const) {
    await check(`GPUI focus preserved during ${operation}`, async () => {
      await control('focus-gpui');
      const before = await poll('Native keyboard returned to GPUI', requireState, value => value.gpuiFocused && value.pages.every(page => !page.nativeFocused));
      if (operation === 'read') await alpha.locator('body').ariaSnapshot();
      if (operation === 'navigate') await alpha.goto(`${origin}/page/alpha?run=${runId}`);
      if (operation === 'click') await alpha.locator('#count').click();
      if (operation === 'fill') await alpha.locator('#name').fill('Focus check');
      await Bun.sleep(400);
      const value = await requireState();
      assert(value.gpuiFocused && value.pages.every(page => !page.nativeFocused), 'Operation took the native keyboard away from the GPUI sentinel');
      for (const page of value.pages) {
        assert.equal(page.nativeFocusGains, before.pages.find(previous => previous.id === page.id)?.nativeFocusGains, 'Operation transiently acquired native keyboard focus');
      }
    });
  }
  await check('console and failed network diagnostics', async () => {
    const consoleEvents: string[] = [];
    const failures: string[] = [];
    const onConsole = (message: import('playwright-core').ConsoleMessage) => {
      if (message.text() === 'fintwind-poc-console') consoleEvents.push(message.text());
    };
    const onResponse = (response: import('playwright-core').Response) => {
      if (response.url().startsWith(origin) && response.status() === 404) failures.push(`${response.status()} ${new URL(response.url()).pathname}`);
    };
    alpha.on('console', onConsole);
    alpha.on('response', onResponse);
    try {
      await alpha.locator('#debug').click();
      await poll('Fixture diagnostics', async () => ({ consoleEvents, failures }), value => value.consoleEvents.length > 0 && value.failures.length > 0);
      await writeFile(join(output, 'diagnostics.json'), JSON.stringify({ consoleEvents, failures }, null, 2));
    } finally {
      alpha.off('console', onConsole);
      alpha.off('response', onResponse);
    }
  });
  await check('popup follows existing in-place host policy', async () => {
    await alpha.locator('#popup').click();
    await poll('Popup navigation', async () => alpha.url(), value => new URL(value).pathname === '/popup');
    await poll('Host popup URL/title', requireState, value => value.pages.some(page => page.id === 'alpha' && page.url === alpha.url() && page.title === 'Fintwind PoC Popup'));
    assert.equal(context.pages().length, 3, 'Popup did not create an orphan target');
    await alpha.goto(`${origin}/page/alpha?run=${runId}`);
  });
  if (args.has('--screenshots')) {
    await check('optional screenshot API artifact', async () => {
      const bytes = await alpha.screenshot({ path: join(output, 'alpha.png'), timeout: 10_000 });
      assert(bytes.length > 100);
      return `SHA256 ${createHash('sha256').update(bytes).digest('hex')}; no visual comparison performed.`;
    });
  } else {
    report.checks.push({ name: 'optional screenshot API artifact', status: 'skipped', details: 'Requires --screenshots; no screenshot or visual test requested.', durationMs: 0 });
  }
  await check('disconnect leaves native pages alive; reconnect works', async () => {
    await browser!.close();
    browser = undefined;
    const value = await requireState();
    assert.equal(value.pages.filter(page => page.ready).length, 3);
    assert(endpoint);
    browser = await chromium.connectOverCDP(endpoint, { noDefaults: true, isLocal: true, timeout: 10_000 });
    browser.contexts()[0]?.setDefaultTimeout(8000);
    browser.contexts()[0]?.setDefaultNavigationTimeout(10_000);
    assert.equal(browser.contexts()[0]?.pages().length, 3);
    const current = browser.contexts()[0]!.pages().find(page => page.url() === alpha.url());
    assert(current, 'Reconnected to the original alpha target');
    await current.locator('#name').fill('Reconnected');
    assert.equal(await current.locator('#name-value').textContent(), 'Reconnected');
  });
  await check('host closes beta without an orphan; verified remaining pages stay usable', async () => {
    await control('close-page', 'beta');
    await poll('CDP beta target removed', async () => browser!.contexts()[0]!.pages(), value => value.length === 2 && value.every(page => new URL(page.url()).pathname !== '/page/beta'));
    const value = await requireState();
    assert.deepEqual(value.pages.map(page => page.id).sort(), ['alpha', 'gamma']);
    const remainingAlpha = browser!.contexts()[0]!.pages().find(page => new URL(page.url()).pathname === '/page/alpha');
    assert(remainingAlpha);
    const before = Number(await remainingAlpha.locator('#count-value').textContent());
    await remainingAlpha.locator('#count').click();
    assert.equal(await remainingAlpha.locator('#count-value').textContent(), String(before + 1));
    // Only attribute a gamma failure to closure if its input worked before it.
    // Otherwise the earlier capability failure already explains the limitation.
    if (report.checks.find(check => check.name === 'all three visible native surfaces accept input')?.status === 'passed') {
      const gamma = browser!.contexts()[0]!.pages().find(page => new URL(page.url()).pathname === '/page/gamma');
      assert(gamma);
      const gammaBefore = Number(await gamma.locator('#count-value').textContent());
      await gamma.locator('#count').click();
      assert.equal(await gamma.locator('#count-value').textContent(), String(gammaBefore + 1));
      return 'Beta target was removed; both previously working alpha and gamma remained mapped and accepted input.';
    }
    return 'Beta target was removed and alpha stayed usable. Gamma input after closure is unverified because its baseline input check failed.';
  });
} catch (error) {
  report.errors.push(error instanceof Error ? error.stack ?? error.message : String(error));
  console.error('[X] Probe could not complete:', error);
} finally {
  if (browser) {
    try { await deadline(browser.close(), 'Disconnect cleanup', 5000); }
    catch (error) { report.errors.push(`Disconnect cleanup: ${String(error)}`); }
  }
  if (host && host.exitCode === null && host.signalCode === null) {
    try {
      const exited = exit(host);
      await control('shutdown');
      await deadline(exited, 'Host shutdown', 5000);
    } catch {
      // The exact PID was created by this runner; never kill other Fintwind processes.
      if (host.pid && host.exitCode === null && host.signalCode === null) {
        const kill = spawn('taskkill', ['/PID', String(host.pid), '/T', '/F'], { shell: false, windowsHide: true, stdio: 'ignore' });
        try { assert.equal(await deadline(exit(kill), 'Host process cleanup', 5000), 0); }
        catch (error) { report.errors.push(`Process cleanup: ${String(error)}`); }
      }
    }
  }
  if (endpoint) {
    try {
      await deadline((async () => {
        const until = Date.now() + 4500;
        while (Date.now() < until) {
          try { await fetch(`${endpoint}/json/version`, { signal: AbortSignal.timeout(500) }); }
          catch { return; }
          await Bun.sleep(100);
        }
        throw new Error('The isolated browser is still listening after host shutdown');
      })(), 'CDP endpoint shutdown', 5000);
    } catch (error) { report.errors.push(`CDP cleanup: ${String(error)}`); }
  }
  fixture?.stop();
  process.off('SIGINT', interrupt);
  process.off('SIGTERM', interrupt);
  if (interrupted && !report.errors.includes('Probe interrupted.')) report.errors.push('Probe interrupted.');
  report.finishedAt = new Date().toISOString();
  report.status = report.errors.length || report.checks.some(check => check.status === 'failed') ? 'failed' : 'passed';
  await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2));
  console.log(`Report: ${join(output, 'report.json')}`);
  process.exitCode = report.status === 'passed' ? 0 : 1;
}
