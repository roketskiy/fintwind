// Integration check: Rust projections/deltas/AES -> phone crypto/validation/state.
// No server, child cleanup, installed app, personal data, or real pairing.
import { resolve } from "node:path";
import { mkdir } from "node:fs/promises";
import { SessionUpdates } from "../mobile/src/lib/session-updates";
import { deriveKeys, openEnvelope } from "../mobile/src/lib/remote-crypto";
import { parseResult } from "../mobile/src/lib/remote-validation";

const root = resolve(import.meta.dir, "..");
const artifacts = resolve(root, "target/remote-stream-blackbox", `run-${crypto.randomUUID()}`);
await mkdir(artifacts, { recursive: true });
const executable = resolve(root, "target/debug/examples", process.platform === "win32" ? "stream_fixture.exe" : "stream_fixture");
if (!(await Bun.file(executable).exists())) throw new Error("First build: cargo build --locked --package fintwind-remote --example stream_fixture");
// The prebuilt fixture is a pure computation process with no descendants.
// Do not make a timed-out test responsible for cleaning up Cargo/rustc children.
const fixture = Bun.spawnSync([executable], {
  cwd: artifacts, timeout: 10000, stdout: "pipe", stderr: "pipe",
  env: Object.fromEntries(["PATH", "SystemRoot", "TEMP", "TMP"].flatMap(key => process.env[key] ? [[key, process.env[key]!]] : [])),
});
if (fixture.exitCode !== 0) throw new Error(new TextDecoder().decode(fixture.stderr));
const bundle = JSON.parse(new TextDecoder().decode(fixture.stdout));
const key = await crypto.subtle.importKey("raw", new Uint8Array(32).fill(7), "HKDF", false, ["deriveKey"]);
const [, receiveKey] = await deriveKeys(key, new Uint8Array(32).fill(11), new Uint8Array(32).fill(17));
const updates = new SessionUpdates(bundle.sessionId);
let previous: ReturnType<SessionUpdates["apply"]> | null = null;
let reused = 0;
let maximumApplyMs = 0;
const permissionModes = new Set<string>();
const plainFrames: Record<string, unknown>[] = [];
for (const [index, sealed] of bundle.frames.entries()) {
  const frame = await openEnvelope(sealed, receiveKey, index) as Record<string, unknown>;
  plainFrames.push(frame);
  const start = performance.now();
  const detail = updates.apply(frame);
  if (detail.runtimeMode !== bundle.expected[index].runtimeMode) throw new Error(`Permission projection mismatch at ${index}`);
  if (detail.runtimeMode) permissionModes.add(detail.runtimeMode);
  maximumApplyMs = Math.max(maximumApplyMs, performance.now() - start);
  if (JSON.stringify(detail) !== JSON.stringify(parseResult("getSession", bundle.expected[index]))) throw new Error(`Projection mismatch at ${index}`);
  if (previous && index !== 30 && detail.entries?.[0] === previous.entries?.[0]) reused++;
  previous = detail;
}
function rejected(frame: Record<string, unknown>, name: string) {
  const parser = new SessionUpdates(bundle.sessionId);
  parser.apply(plainFrames[0]!);
  let failed = false;
  try { parser.apply(frame); } catch { failed = true; }
  if (!failed) throw new Error(`Accepted invalid ${name}`);
}
const second = plainFrames[1]!;
rejected({ ...second, baseRevision: "0".repeat(64) }, "base revision");
rejected({ ...second, edits: [{ path: ["__proto__", "polluted"], set: true }] }, "prototype path");
rejected({ ...second, edits: [{ path: ["entries", "999999"], set: {} }] }, "array growth");
rejected({ ...second, edits: [{ path: ["entries", "121", "text"], append: "x".repeat(65537) }] }, "oversized text");
rejected({ ...second, revision: "invalid" }, "revision format");
rejected({ ...second, edits: [{ path: ["runtimeMode"], set: "auto" }, { path: ["revision"], set: second.revision }] }, "permission mode");
let replayRejected = false;
try { await openEnvelope(bundle.frames[1], receiveKey, 2); } catch { replayRejected = true; }
if (!replayRejected || reused < 50 || permissionModes.size !== 3) throw new Error("Replay, structural sharing or permission round-trip failed");
const report = { ...bundle.metrics, receiverMaximumApplyMs: maximumApplyMs, reusedHistoryFrames: reused,
  invalidCasesRejected: 7, permissionModes: [...permissionModes], plaintextReduction: 1 - bundle.metrics.updateBytes / bundle.metrics.fullSnapshotBytes,
  scope: "synthetic Rust-to-TypeScript encrypted codec integration; not a live desktop/relay test" };
await Bun.write(resolve(artifacts, "report.json"), JSON.stringify(report, null, 2));
await Bun.write(resolve(artifacts, "browser-fixture.json"), JSON.stringify({ sessionId: bundle.sessionId, frames: plainFrames }));
console.log(JSON.stringify({ ...report, artifactDirectory: artifacts }, null, 2));
