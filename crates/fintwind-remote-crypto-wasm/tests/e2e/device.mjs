// E2E device peer for the wasm crypto bridge.
//
// Loads the REAL wasm artifact built by wasm-pack (the same `no-modules`
// glue shape the PWA ships) and drives it over stdin/stdout JSON lines,
// exactly the way the PWA's crypto-bridge calls it. The Rust test
// (tests/e2e_remote_link.rs) is the host peer on the other end.
//
// Stdout carries protocol lines only; everything else goes to stderr.

import fs from "node:fs";
import readline from "node:readline";

const glueDir = process.argv[2];
if (!glueDir) {
  console.error("usage: node device.mjs <glueDir>");
  process.exit(2);
}

// The no-modules glue keeps its bindings in a top-level `let` (that is
// why the PWA needs publish.js); evaluate it in global scope, take the
// binding, then initialize synchronously from the wasm bytes.
const glue = fs.readFileSync(`${glueDir}/fintwind-crypto.js`, "utf8");
const wasmBytes = fs.readFileSync(`${glueDir}/fintwind-crypto_bg.wasm`);
const api = (0, eval)(`${glue}\n;wasm_bindgen`);
api.initSync({ module: wasmBytes });
const { Identity, Pairing, WasmChannel } = api;

const objects = new Map();
let nextId = 1;

function handle(msg) {
  switch (msg.cmd) {
    case "identity_new": {
      const id = String(nextId++);
      const identity = new Identity();
      objects.set(id, identity);
      return {
        id,
        identity: identity.to_json(),
        fingerprint: identity.noise_fingerprint(),
        signing: identity.signing_public_b64(),
        noise: identity.noise_public_b64(),
      };
    }
    case "identity_load": {
      const identity = Identity.from_json(msg.identity);
      objects.set(msg.id, identity);
      return {};
    }
    case "pairing_start": {
      const pairing = new Pairing(msg.identity, msg.invite);
      objects.set(msg.id, pairing);
      return { step0: pairing.step0_b64(), host_id: pairing.host_id() };
    }
    case "pairing_finish": {
      const pairing = objects.get(msg.id);
      const accepted = pairing.finish(msg.reply, msg.device_id, msg.device_name);
      const channel = accepted.take_channel();
      objects.set(msg.channel_id, channel);
      return {
        message3: accepted.message3_b64(),
        check_code: accepted.pairing_check_code(),
        host_fingerprint: accepted.host_fingerprint(),
      };
    }
    case "channel_seal": {
      const channel = objects.get(msg.id);
      // wasm-bindgen maps u64 to BigInt, so `seq` must cross as one.
      return { frame: channel.seal_frame(BigInt(msg.seq), msg.body) };
    }
    case "channel_open": {
      const channel = objects.get(msg.id);
      return { body: channel.open_frame(msg.frame) };
    }
    case "channel_info": {
      const channel = objects.get(msg.id);
      return {
        fingerprint: channel.remote_fingerprint(),
        static_b64: channel.remote_static_b64(),
      };
    }
    case "connect_start": {
      const connect = WasmChannel.connect(
        msg.identity,
        msg.host_public,
        msg.host_id,
        msg.device_id,
      );
      objects.set(msg.id, connect);
      return { step0: connect.step0_b64() };
    }
    case "connect_finish": {
      const connect = objects.get(msg.id);
      const channel = connect.finish(msg.reply);
      objects.set(msg.channel_id, channel);
      return {
        fingerprint: channel.remote_fingerprint(),
        static_b64: channel.remote_static_b64(),
      };
    }
    default:
      throw new Error(`unknown command: ${msg.cmd}`);
  }
}

function respond(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

const rl = readline.createInterface({ input: process.stdin });
for await (const line of rl) {
  if (!line.trim()) continue;
  let msg;
  try {
    msg = JSON.parse(line);
  } catch (error) {
    respond({ ok: false, error: `bad json: ${error.message}` });
    continue;
  }
  try {
    respond({ ok: true, ...handle(msg) });
  } catch (error) {
    respond({ ok: false, error: error?.message ?? String(error) });
  }
}
