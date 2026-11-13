import { decodeBase64Url, deriveKeys, encodeBase64Url, openEnvelope, sealRecord } from "../../../mobile/src/lib/remote-crypto";

const binary = Bun.argv[2];
if (!binary) throw new Error("checker path required");
const env: Record<string, string> = {};
for (const name of ["SystemRoot", "WINDIR", "TEMP", "TMP"]) {
  const value = process.env[name];
  if (value) env[name] = value;
}
const child = Bun.spawn([binary], { env, stdout: "pipe", stderr: "pipe" });
try {
  const reader = child.stdout.getReader();
  const start = await reader.read();
  if (start.done) throw new Error("loopback server did not start");
  const config: unknown = JSON.parse(new TextDecoder().decode(start.value));
  if (typeof config !== "object" || config === null || !("address" in config) || !("secret" in config) || typeof config.address !== "string" || typeof config.secret !== "string") throw new Error("invalid loopback configuration");
  const socket = new WebSocket(`ws://${config.address}/__fintwind_remote__`);
  const messages: string[] = [];
  let wake: (() => void) | undefined;
  socket.onmessage = event => { messages.push(String(event.data)); wake?.(); };
  const next = async (): Promise<Record<string, unknown>> => {
    while (messages.length === 0) await new Promise<void>(resolve => { wake = resolve; });
    wake = undefined;
    const text = messages.shift();
    if (!text) throw new Error("empty loopback frame");
    return JSON.parse(text);
  };
  await new Promise<void>((resolve, reject) => { socket.onopen = () => resolve(); socket.onerror = () => reject(new Error("loopback connection failed")); });
  const phone = crypto.getRandomValues(new Uint8Array(32));
  socket.send(JSON.stringify({ type: "hello", version: 1, nonce: encodeBase64Url(phone) }));
  const hello = await next();
  if (hello.type !== "hello" || hello.version !== 1 || typeof hello.nonce !== "string") throw new Error("invalid hello");
  const secret = await crypto.subtle.importKey("raw", decodeBase64Url(config.secret), "HKDF", false, ["deriveKey"]);
  const [client, server] = await deriveKeys(secret, phone, decodeBase64Url(hello.nonce));
  const ready = await openEnvelope(await next(), server, 0);
  if (JSON.stringify(ready) !== '{"type":"ready"}') throw new Error("invalid ready");
  socket.send(await sealRecord(client, 0, { type: "ping" }));
  const pong = await openEnvelope(await next(), server, 1);
  if (JSON.stringify(pong) !== '{"type":"pong"}') throw new Error("invalid response");
  socket.close();
  if (await child.exited !== 0) throw new Error("loopback checker failed");
  console.log("encrypted WebSocket loopback passed");
} finally {
  if (child.exitCode === null) child.kill();
}
