import { deriveKeys, encodeBase64Url, openEnvelope, sealRecord } from "../../../mobile/src/lib/remote-crypto";

const binary = Bun.argv[2];
if (!binary) throw new Error("expected the absolute path to the Go checker");
const secret = crypto.getRandomValues(new Uint8Array(32));
const phone = crypto.getRandomValues(new Uint8Array(32));
const desktop = crypto.getRandomValues(new Uint8Array(32));
const key = await crypto.subtle.importKey("raw", secret, "HKDF", false, ["deriveKey"]);
const [client, server] = await deriveKeys(key, phone, desktop);
const frame = JSON.parse(await sealRecord(client, 0, { type: "ping" }));
const input = JSON.stringify({
  Secret: encodeBase64Url(secret),
  PhoneNonce: encodeBase64Url(phone),
  DesktopNonce: encodeBase64Url(desktop),
  Frame: frame,
});
const env: Record<string, string> = {};
for (const name of ["SystemRoot", "WINDIR", "TEMP", "TMP"]) {
  const value = process.env[name];
  if (value) env[name] = value;
}
const child = Bun.spawnSync([binary], { stdin: Buffer.from(input), stdout: "pipe", stderr: "pipe", env });
if (child.exitCode !== 0) throw new Error(new TextDecoder().decode(child.stderr));
const response = JSON.parse(new TextDecoder().decode(child.stdout));
const opened = await openEnvelope(response, server, 0);
if (JSON.stringify(opened) !== JSON.stringify({ type: "ready" })) throw new Error("desktop frame did not decrypt");
console.log("mobile and desktop frames interoperate; replay rejected");
