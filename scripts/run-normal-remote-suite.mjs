// Sequential normal E2E batch. Invoke from an independent hidden entry.
// Every process tree is contained by the proven supervisor and observer.
import fs from 'node:fs/promises';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';

const base = 'C:/Users/Public/Temp/opencode';
const root = await fs.mkdtemp(`${base}/normal-remote-suite-`);
const runnerSource = `${base}/run-owned-real-chain-20261011-extended.mjs`;
const runner = path.join(root, 'run-chain.mjs');
const sourceScript = fileURLToPath(import.meta.url);
const repo = path.resolve(path.dirname(sourceScript), '..');
const pwa = path.resolve(repo, '../fintwind-pwa');
const sha = async (file) => createHash('sha256').update(await fs.readFile(file)).digest('hex');
const load = async (file) => JSON.parse(await fs.readFile(file, 'utf8'));
const report = { artifact: 'normal-remote-suite', passed: false, batchPlanVerified: false,
  fullPlanVerified: false, publicChainVerified: false, maliciousTestsEnabled: false,
  startedAt: new Date().toISOString(), checks: [], scenarios: [],
  notCovered: [
    'real OpenCode worktree snapshot restoration',
    'in-flight cancellation and query-failure retry',
    'original running-task unknown settlement after daemon crash',
    'relay stress: old attack and slow-connection runners remain disabled',
    'real phone WASM timing and non-debug production performance',
    'public cross-origin chain: user deployment required',
    'malicious scenarios: disabled by user decision',
  ] };
const allScenarios = [
  ['completed-management', ['--session-management']],
  ['busy-archive', ['--archive-running']],
  ['started-delete', ['--delete-started']],
  ['explicit-stop', ['--cancel-running']],
  ['phone-queue-reload', ['--queue-reload']],
  ['host-safe-restart', ['--host-restart', '--host-auto-reconnect']],
  ['host-crash', ['--host-restart', '--host-auto-reconnect', '--host-crash']],
  ['daemon-crash-cancel-restart', ['--host-restart', '--host-auto-reconnect', '--daemon-crash', '--daemon-only', '--cancel-result-restart']],
  ['both-crash-cancel-restart', ['--host-restart', '--host-auto-reconnect', '--host-crash', '--daemon-crash', '--cancel-result-restart']],
  ['two-device-approval', ['--multi-device', '--queue-reload']],
  ['two-host-switching', ['--multi-host', '--multi-device', '--queue-reload']],
  ['idle-and-foreground-liveness', ['--idle-liveness', '--foreground-liveness']],
  ['explicit-worker-update', ['--service-worker-update']],
  ['history-fork-rewind', ['--history-operations']],
  ['full-relationship-matrix', ['--multi-host', '--multi-device', '--queue-reload', '--relationship-matrix']],
  ['normal-wasm-measurement', ['--normal-crypto']],
  ['session-options-apply', ['--session-options']],
];
const requested = process.argv.slice(2);
if (requested.length && (requested[0] !== '--only' || requested.length < 2
  || new Set(requested.slice(1)).size !== requested.length - 1
  || requested.slice(1).some((name) => !allScenarios.some(([candidate]) => candidate === name)))) {
  throw new Error('Choose explicit normal scenario names after --only.');
}
const scenarios = requested.length ? allScenarios.filter(([name]) => requested.slice(1).includes(name)) : allScenarios;
report.scope = requested.length ? 'targeted-normal-recheck' : 'full-defined-normal-batch';
report.omittedScenarios = allScenarios.filter(([name]) => !scenarios.some(([candidate]) => candidate === name)).map(([name]) => name);
report.plannedScenarios = scenarios.map(([name, args]) => ({ name, args }));
async function inventory(directory, prefix = '') {
  const result = {};
  for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
    if (['target', 'node_modules', '.git'].includes(entry.name)) continue;
    const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) Object.assign(result, await inventory(absolute, relative));
    else if (entry.isFile()) result[relative] = await sha(absolute);
    else throw new Error('unsupported-source-input');
  }
  return Object.fromEntries(Object.entries(result).sort());
}
const inputRoots = [path.join(repo, 'crates'), path.join(repo, 'locales'), path.join(repo, 'db/migrations'),
  path.join(repo, 'scripts/fixtures/remote-control'), path.join(pwa, 'src'), path.join(pwa, 'public'), path.join(pwa, 'scripts')];
async function sourceState() {
  const state = {};
  for (const directory of inputRoots) state[directory] = await inventory(directory);
  for (const file of [path.join(repo, 'Cargo.toml'), path.join(repo, 'Cargo.lock'), path.join(repo, 'models_thinking_modes.json'),
    path.join(repo, 'resources/opencode-browser-plugin.ts'), path.join(pwa, 'package.json'), path.join(pwa, 'bun.lock'),
    path.join(pwa, 'vite.config.ts'), path.join(pwa, 'index.html'), sourceScript, runnerSource]) state[file] = await sha(file);
  return JSON.stringify(state);
}
async function invoke(name, args, env) {
  const started = Date.now();
  const child = spawn(process.execPath, [runner, ...args], { cwd: root, env,
    windowsHide: true, detached: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  child.stdout.on('data', (bytes) => { output = (output + bytes.toString()).slice(-8192); });
  child.stderr.resume(); // Discard; never persist raw exception output.
  const exitCode = await new Promise((resolve) => { child.once('error', () => resolve(null)); child.once('close', resolve); });
  const match = output.match(/\[ARTIFACT\] (C:[/\\]Users[/\\]Public[/\\]Temp[/\\]opencode[/\\]owned-real-chain-frozen-[A-Za-z0-9]+[/\\]summary\.json)/);
  const artifact = match?.[1] ?? null;
  const result = artifact ? await load(artifact).catch(() => null) : null;
  const row = { name, args, exitCode, elapsedMs: Date.now() - started, artifact, passed: false,
    cleanupVerified: result?.containmentVerified === true, businessProcessesStarted: result?.businessProcessesStarted ?? null };
  if (name === 'independent-preflight') {
    row.passed = exitCode === 0 && result?.artifact === 'owned-normal-preflight'
      && result.passed === true && result.independentEntryVerified === true && result.businessProcessesStarted === false;
  } else if (result) {
    const ownership = await load(result.ownershipReport).catch(() => null);
    const chain = await load(result.chainReport).catch(() => null);
    const preflight = await load(result.ownershipPreflight).catch(() => null);
    row.cleanupVerified = result.containmentVerified === true && result.allOwnedProcessesStopped === true
      && ownership?.runnerInAnyJob === false && ownership.cleanup?.unverified?.length === 0
      && ownership.outsider?.aliveBeforeTeardown === true && ownership.outsider?.exitedWithinDeadline === true;
    row.passed = exitCode === 0 && result.passed === true && result.inputsUnchanged === true
      && result.normalOnly === true && result.maliciousWireRequested === false && result.cleanupRevoked === true
      && row.cleanupVerified && chain?.passed === true && chain.checks.every((check) => check.passed === true)
      && ownership.summary?.failed === 0 && ownership.summary?.blocked === 0
      && ownership.scenarios.every((scenario) => scenario.outcome === 'pass'
        && scenario.checks.every((check) => check.verdict === 'verified'))
      && preflight?.runnerInAnyJob === false && preflight.summary?.failed === 0;
    row.chainChecks = chain?.checks?.length ?? 0;
    row.failedPhase = chain?.failedPhase ?? result.blocked ?? null;
    row.artifactSha256 = await sha(artifact);
  }
  console.log(`[CASE] ${name}: ${row.passed ? 'pass' : 'fail'}`);
  report.scenarios.push(row);
  await fs.writeFile(path.join(root, 'progress.json'), JSON.stringify(report, null, 2));
  return row;
}
try {
  await fs.copyFile(sourceScript, path.join(root, 'run-suite.mjs'));
  await fs.copyFile(runnerSource, runner);
  report.suiteSha256 = await sha(path.join(root, 'run-suite.mjs'));
  report.runnerSha256 = await sha(runner);
  const before = await sourceState();
  report.sourceStateSha256 = createHash('sha256').update(before).digest('hex');
  const home = path.join(root, 'home');
  await fs.mkdir(home);
  const env = {};
  for (const name of ['SystemRoot', 'windir', 'COMSPEC', 'PATH', 'PATHEXT', 'NUMBER_OF_PROCESSORS']) {
    if (process.env[name]) env[name] = process.env[name];
  }
  for (const name of ['HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP']) env[name] = home;
  const preflight = await invoke('independent-preflight', ['--preflight-only'], env);
  if (!preflight.passed) {
    report.blocked = 'independent-entry-preconditions';
  } else {
    const deadline = Date.now() + 30 * 60_000;
    for (const [name, args] of scenarios) {
      if (Date.now() > deadline) { report.blocked = 'batch-deadline'; break; }
      if (await sourceState() !== before || await sha(runner) !== report.runnerSha256) {
        report.blocked = 'input-drift'; break;
      }
      const row = await invoke(name, args, env);
      if (!row.cleanupVerified) { report.blocked = 'incomplete-cleanup-or-preparation'; break; }
    }
  }
  report.inputsUnchanged = await sourceState() === before && await sha(runner) === report.runnerSha256;
  report.batchPlanVerified = !report.blocked && report.inputsUnchanged && report.scenarios.length === scenarios.length + 1
    && report.scenarios.every((row) => row.passed);
  report.passed = report.batchPlanVerified;
} catch {
  report.failure = 'normal-suite-preparation-or-evidence-failed';
} finally {
  report.finishedAt = new Date().toISOString();
  report.results = { passed: report.scenarios.filter((row) => row.passed).length,
    failed: report.scenarios.filter((row) => !row.passed).length,
    notRun: scenarios.length + 1 - report.scenarios.length };
  report.unverifiedScenarios = allScenarios.filter(([name]) => !report.scenarios.some((row) => row.name === name && row.passed))
    .map(([name]) => name);
  await fs.writeFile(path.join(root, 'summary.json'), JSON.stringify(report, null, 2), { flag: 'wx' });
  console.log(`[SUITE] ${path.join(root, 'summary.json')}`);
  process.exitCode = report.passed ? 0 : report.blocked ? 2 : 1;
}
