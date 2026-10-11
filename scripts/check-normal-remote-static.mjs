// Compile/type/contract checks only. Never starts an acceptance service.
import fs from 'node:fs/promises';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const pwa = path.resolve(repo, '../fintwind-pwa');
const root = await fs.mkdtemp('C:/Users/Public/Temp/opencode/normal-remote-static-');
const sha = async (file) => createHash('sha256').update(await fs.readFile(file)).digest('hex');
const report = { artifact: 'normal-remote-static', passed: false, runtimeChainVerified: false,
  fullPlanVerified: false, publicChainVerified: false, startedAt: new Date().toISOString(), checks: [] };
async function tree(directory, prefix) {
  const result = {};
  for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
    if (['target', 'node_modules', '.git'].includes(entry.name)) continue;
    const relative = `${prefix}/${entry.name}`;
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) Object.assign(result, await tree(absolute, relative));
    else if (entry.isFile()) result[relative] = await sha(absolute);
    else throw new Error('unsupported-input');
  }
  return result;
}
async function inputs() {
  const result = {};
  for (const directory of ['crates', 'locales', 'db/migrations', 'scripts/fixtures/remote-control']) {
    Object.assign(result, await tree(path.join(repo, directory), `native/${directory}`));
  }
  for (const relative of ['Cargo.toml', 'Cargo.lock', 'models_thinking_modes.json', 'resources/opencode-browser-plugin.ts']) {
    result[`native/${relative}`] = await sha(path.join(repo, relative));
  }
  for (const directory of ['src', 'public', 'scripts']) Object.assign(result, await tree(path.join(pwa, directory), `pwa/${directory}`));
  for (const relative of ['package.json', 'bun.lock', 'tsconfig.json', 'vite.config.ts', 'index.html']) {
    result[`pwa/${relative}`] = await sha(path.join(pwa, relative));
  }
  return Object.fromEntries(Object.entries(result).sort());
}
const env = {};
for (const name of ['PATH', 'PATHEXT', 'SystemRoot', 'windir', 'COMSPEC', 'SYSTEMDRIVE', 'NUMBER_OF_PROCESSORS',
  'HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN',
  'INCLUDE', 'LIB', 'LIBPATH', 'ProgramFiles', 'ProgramFiles(x86)', 'ProgramData',
  'VCToolsInstallDir', 'VCINSTALLDIR', 'VSINSTALLDIR', 'WindowsSdkDir', 'WindowsSDKVersion', 'UniversalCRTSdkDir']) {
  if (process.env[name]) env[name] = process.env[name];
}
report.environmentWhitelistNames = Object.keys(env).sort();
async function check(name, command, args, cwd, allowedFailure = false) {
  const started = Date.now();
  const child = spawn(command, args, { cwd, env, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stdout.resume(); child.stderr.resume();
  const code = await new Promise((resolve) => {
    child.once('error', () => resolve(null));
    child.once('close', resolve);
  });
  report.checks.push({ name, command: [command, ...args], exitCode: code,
    elapsedMs: Date.now() - started, passed: code === 0, behavioralAcceptance: false });
  console.log(`[CHECK] ${name}: ${code === 0 ? 'pass' : 'fail'}`);
  if (code !== 0 && !allowedFailure) report.failedChecks = (report.failedChecks ?? 0) + 1;
}
try {
  report.phase = 'input-inventory';
  report.inputSha256 = await inputs();
  report.phase = 'compile-type-contract';
  await check('native workspace', 'cargo', ['check', '--locked', '--workspace'], repo);
  await check('existing test targets compile only', 'cargo', ['check', '--locked', '--workspace', '--tests'], repo);
  await check('remote examples compile only', 'cargo', ['check', '--locked', '-p', 'fintwind-remote-host', '--examples'], repo);
  await check('WASM target compile only', 'cargo', ['check', '--locked', '-p', 'fintwind-remote-crypto-wasm', '--target', 'wasm32-unknown-unknown'], repo);
  await check('PWA type check', process.execPath, [path.join(pwa, 'node_modules/typescript/bin/tsc'), '-b'], pwa);
  await check('Rust PWA contract drift', process.execPath, [path.join(pwa, 'scripts/generate-remote-contract.mjs'), '--check'], pwa);
  for (const relative of ['owned-real-chain-driver.mjs', 'browser-ownership-probe.mjs', 'owned-lifetime-parent.mjs']) {
    await check(`${relative} syntax`, process.execPath, ['--check', path.join(pwa, 'scripts', relative)], pwa);
  }
  await check('synthetic provider syntax only', 'E:/Python312/python.exe', ['-c',
    'import ast, pathlib; ast.parse(pathlib.Path("scripts/fixtures/remote-control/synthetic_opencode.py").read_text(encoding="utf-8"))'], repo);
  report.inputsUnchanged = JSON.stringify(report.inputSha256) === JSON.stringify(await inputs());
  report.passed = !report.failedChecks && report.inputsUnchanged;
  report.phase = report.passed ? null : 'check-or-input-drift';
} catch {
  report.failure = 'static-check-preparation-or-evidence-failed';
} finally {
  report.finishedAt = new Date().toISOString();
  const frozen = path.join(root, 'check-static.mjs');
  await fs.copyFile(fileURLToPath(import.meta.url), frozen);
  report.runnerSha256 = await sha(frozen);
  await fs.writeFile(path.join(root, 'summary.json'), JSON.stringify(report, null, 2), { flag: 'wx' });
  console.log(`[ARTIFACT] ${path.join(root, 'summary.json')}`);
  process.exitCode = report.passed ? 0 : 1;
}
