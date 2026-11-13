import { createServer } from "node:net";
import { join } from "node:path";
import { decodeBase64Url, deriveKeys, encodeBase64Url, makeNonce, openEnvelope, sealRecord } from "../../../mobile/src/lib/remote-crypto";
import { parseResult } from "../../../mobile/src/lib/remote-validation";

const binary = Bun.argv[2];
const fixture = Bun.argv[3];
if (!binary || !fixture) throw new Error("checker and isolated relay paths required");
const node = Bun.which("node");
if (!node) throw new Error("node unavailable");
const probe = createServer();
await new Promise<void>((resolve, reject) => probe.once("error", reject).listen(0, "127.0.0.1", resolve));
const address = probe.address();
if (!address || typeof address === "string") throw new Error("port unavailable");
const port = address.port;
await new Promise<void>(resolve => probe.close(() => resolve()));
const revoked = join(fixture, "revoked.json");
await Bun.write(revoked, "[]");
const env: Record<string, string> = {};
for (const name of ["SystemRoot", "WINDIR", "TEMP", "TMP"]) {
  const value = process.env[name];
  if (value) env[name] = value;
}
const relay = Bun.spawn([node, join(fixture, "index.mjs")], {
  cwd: fixture,
  env: { ...env, HOST: "127.0.0.1", PORT: String(port), RELAY_PUBLIC_ADMISSION: "1", RELAY_TOKEN_SECRET: "test-fixture-only-never-use-in-production-321", RELAY_REVOKED_FILE: revoked },
  stdout: "pipe", stderr: "pipe",
});
let host: ReturnType<typeof Bun.spawn> | undefined;
try {
  const relayReader = relay.stdout.getReader();
  const started = await relayReader.read();
  if (started.done || !new TextDecoder().decode(started.value).includes("listening")) throw new Error("isolated relay did not start");
  host = Bun.spawn([binary, `http://127.0.0.1:${port}`], { env, stdout: "pipe", stderr: "pipe" });
  const hostReader = host.stdout.getReader();
  const output = await hostReader.read();
  if (output.done) throw new Error("host did not register");
  const pair: unknown = JSON.parse(new TextDecoder().decode(output.value));
  if (!pair || typeof pair !== "object" || !("route" in pair) || !("key" in pair) || typeof pair.route !== "string" || typeof pair.key !== "string") throw new Error("invalid pair");
  const socket = new WebSocket(`ws://127.0.0.1:${port}/c/${pair.route}/__fintwind_remote__`);
  const messages: string[] = [];
  let wake: (() => void) | undefined;
  socket.onmessage = event => { messages.push(String(event.data)); wake?.(); };
  const next = async (): Promise<Record<string, unknown>> => {
    while (messages.length === 0) await new Promise<void>(resolve => { wake = resolve; });
    wake = undefined;
    const text = messages.shift();
    if (!text) throw new Error("empty frame");
    return JSON.parse(text);
  };
  await new Promise<void>((resolve, reject) => { socket.onopen = () => resolve(); socket.onerror = () => reject(new Error("phone connection failed")); });
  const nonce = crypto.getRandomValues(new Uint8Array(32));
  socket.send(JSON.stringify({ type: "hello", version: 1, nonce: encodeBase64Url(nonce) }));
  const hello = await next();
  if (hello.type !== "hello" || hello.version !== 1 || typeof hello.nonce !== "string") throw new Error("invalid hello");
  const key = await crypto.subtle.importKey("raw", decodeBase64Url(pair.key), "HKDF", false, ["deriveKey"]);
  const [client, server] = await deriveKeys(key, nonce, decodeBase64Url(hello.nonce));
  const ready = await openEnvelope(await next(), server, 0);
  if (JSON.stringify(ready) !== '{"type":"ready"}') throw new Error("invalid ready");
  const id = crypto.randomUUID();
  socket.send(await sealRecord(client, 0, { id, command: { type: "listWorkspaces" } }));
  const response = await openEnvelope(await next(), server, 1);
  if (!response || typeof response !== "object" || !("id" in response) || response.id !== id || !("ok" in response) || response.ok !== true || !("data" in response)) throw new Error("request failed");
  const mutation = crypto.randomUUID();
  const command = { type: "sendPrompt", sessionId: crypto.randomUUID(), text: "fixture" };
  socket.send(await sealRecord(client, 1, { id: mutation, command }));
  const accepted = await openEnvelope(await next(), server, 2);
  if (!accepted || typeof accepted !== "object" || !("ok" in accepted) || accepted.ok !== true) throw new Error("mutation rejected");
  socket.send(await sealRecord(client, 2, { id: mutation, command }));
  const repeated = await openEnvelope(await next(), server, 3);
  if (!repeated || typeof repeated !== "object" || !("ok" in repeated) || repeated.ok !== false) throw new Error("duplicate mutation accepted");
  const detailID = crypto.randomUUID();
  socket.send(await sealRecord(client, 3, { id: detailID, command: { type: "getSession", sessionId: crypto.randomUUID() } }));
  const detail = await openEnvelope(await next(), server, 4);
  if (!detail || typeof detail !== "object" || !("ok" in detail) || detail.ok !== true || !("data" in detail)) throw new Error("session request failed");
  const parsed = parseResult("getSession", detail.data);
  if (!parsed || typeof parsed !== "object" || !("entries" in parsed) || !Array.isArray(parsed.entries)) throw new Error("session projection invalid");
  const ids = parsed.entries.map(entry => entry.id).join(",");
  if (ids !== "user-message,tool,assistant-message" || /secret-tool-output|secret-provider-prompt/.test(JSON.stringify(detail))) throw new Error("session projection leaked or reordered content");
  socket.send(await sealRecord(client, 4, { id: crypto.randomUUID(), command: { type: "listSessions", projectId: "fixture-project" } }));
  const listing = await openEnvelope(await next(), server, 5);
  if (!listing || typeof listing !== "object" || !("data" in listing)) throw new Error("session listing failed");
  const list = parseResult("listSessions", listing.data);
  if (!list || typeof list !== "object" || !("sessions" in list) || !Array.isArray(list.sessions) || list.sessions.length !== 1 || list.sessions[0].id !== "recent-session") throw new Error("session age limit failed");
  socket.send(await sealRecord(client, 5, { id: crypto.randomUUID(), command: { type: "listSessions", projectId: "later-project" } }));
  const denied = await openEnvelope(await next(), server, 6);
  if (!denied || typeof denied !== "object" || !("ok" in denied) || denied.ok !== false) throw new Error("new workspace was exposed");
  const closed = new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("duplicate JSON was not rejected")), 5000);
    socket.onclose = () => { clearTimeout(timer); resolve(); };
  });
  const duplicate = `{"id":"${crypto.randomUUID()}","command":{"type":"sendPrompt","type":"listWorkspaces"}}`;
  const encrypted = await crypto.subtle.encrypt({ name: "AES-GCM", iv: makeNonce(6), additionalData: new TextEncoder().encode("fintwind.remote.v1"), tagLength: 128 }, client, new TextEncoder().encode(duplicate));
  socket.send(JSON.stringify({ type: "sealed", seq: 6, data: encodeBase64Url(new Uint8Array(encrypted)) }));
  await closed;
  console.log("local relay, encrypted commands, scope, deduplication and session projection passed");
} finally {
  if (host && host.exitCode === null) { host.kill(); await host.exited; }
  if (relay.exitCode === null) { relay.kill(); await relay.exited; }
}
