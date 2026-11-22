import assert from "node:assert/strict";
import { mkdtemp, readFile, unlink, rmdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import plugin from "../resources/opencode-prompt-privacy-plugin.ts";

// Isolated fixture: no OpenCode process, real provider, or user settings.
delete process.env.FINTWIND_PROMPT_PRIVACY_SETTINGS;
const keyDirectory = await mkdtemp(join(tmpdir(), "fintwind-privacy-key-"));
const keyPath = join(keyDirectory, "key");
process.env.FINTWIND_PROMPT_PRIVACY_KEY_PATH = keyPath;
process.env.FINTWIND_OPENCODE_SERVER_PORT = "1";
process.env.OPENCODE_SERVER_PASSWORD = "fixture";
const networkFetch = globalThis.fetch;
const historicalValue = `history-${crypto.randomUUID()}`;
const historyReads = new Map<string, number>();
let failLaterPage = true;
globalThis.fetch = Object.assign(async (input: string | URL | Request, init?: RequestInit) => {
  const url = new URL(input instanceof Request ? input.url : input.toString());
  if (url.host === "127.0.0.1:1" && url.pathname.endsWith("/message")) {
    historyReads.set(url.pathname, (historyReads.get(url.pathname) ?? 0) + 1);
    if (url.pathname.includes("history-partial")) {
      if (!url.searchParams.has("cursor")) return Response.json({ data: [{ type: "user", text: `password="${historicalValue}"` }], cursor: { next: "second" } });
      return failLaterPage ? new Response("error", { status: 500 }) : Response.json({ data: [], cursor: { next: null } });
    }
    if (url.pathname.includes("history-failure")) return new Response("error", { status: 500 });
    if (url.pathname.includes("history-benchmark")) {
      const page = Number(url.searchParams.get("cursor") ?? "0");
      const data = Array.from({ length: 200 }, (_, i) => ({ type: "user", text: i === 199 && page === 49
        ? `password="${historicalValue}"` : `History entry ${page * 200 + i}: ordinary work and result.` }));
      return Response.json({ data, cursor: { next: page < 49 ? String(page + 1) : null } });
    }
    if (url.pathname.includes("history-pages")) {
      if (!url.searchParams.has("cursor")) return Response.json({ data: [{ type: "user", text: "older page" }], cursor: { next: "page-2" } });
      return Response.json({ data: [{ type: "user", text: `password="${historicalValue}"` }], cursor: { next: null } });
    }
    return Response.json({ data: [], cursor: { next: null } });
  }
  return networkFetch(input, init);
}, { preconnect: networkFetch.preconnect }) as typeof fetch;
const hooks = new Map<string, (event: unknown) => unknown>();
function register(name: string, callback: (event: never) => unknown) {
  hooks.set(name, callback as (event: unknown) => unknown);
  return Promise.resolve({ async dispose() { hooks.delete(name); } });
}
const context = {
  session: { hook: register, async context({ sessionID }: { sessionID: string }) {
    return sessionID.startsWith("compacted-") ? [{ type: "compaction", status: "completed" }] : [];
  } }, shell: { hook: register }, tool: { hook: register },
  rpc: {
    register(_definition: unknown, handlers: { resolve(input: unknown): Promise<unknown> }) {
      return register("output.resolve", handlers.resolve as (event: never) => unknown);
    },
  },
};
let dispose = await plugin.setup(context);
const provider = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch(request, server) {
    if (server.upgrade(request)) return;
    return new Response(request.body);
  },
  websocket: { message(socket, frame) { socket.send(frame); } },
});

async function send(transport: "http" | "ws", body: Record<string, unknown>, sessionID: string = transport) {
  if (transport === "http") {
    const event = { sessionID, request: new Request(new URL("/v1/responses", provider.url), {
      method: "POST", body: JSON.stringify(body),
    }) };
    await hooks.get("http.request")!(event);
    return await (await fetch(event.request, { signal: AbortSignal.timeout(3000) })).json();
  }
  const event = { sessionID, frame: JSON.stringify({ type: "response.create", ...body }) };
  await hooks.get("experimental.ws.send")!(event);
  const socket = new WebSocket(`ws://${provider.url.host}`);
  try {
    return await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("WebSocket fixture timed out")), 3000);
      socket.onopen = () => socket.send(event.frame);
      socket.onmessage = message => {
        clearTimeout(timeout);
        resolve(JSON.parse(message.data));
      };
      socket.onerror = () => {
        clearTimeout(timeout);
        reject(new Error("WebSocket fixture failed"));
      };
    });
  } finally {
    socket.close();
  }
}

async function checkReferenceReuse() {
  const sessionID = "reuse-conversation";
  const original = `fixture-${crypto.randomUUID()}`;
  const first = await send("http", { input: [{ role: "user", content: `password="${original}"` }] }, sessionID);
  const ref = /^password="(FINTWIND_SECRET_[A-F0-9]{24})"$/.exec(first.input[0].content)?.[1];
  assert.ok(ref);
  const other = await send("http", { input: [{ role: "user", content: `password="${original}"` }] }, "other-conversation");
  assert.equal(other.input[0].content, first.input[0].content);
  const child = Bun.spawnSync([process.execPath, "-e", `
    import plugin from "./resources/opencode-prompt-privacy-plugin.ts";
    const hooks = new Map();
    const register = async (name, fn) => { hooks.set(name, fn); return { dispose: async () => {} }; };
    globalThis.fetch = async () => Response.json({ data: [], cursor: { next: null } });
    process.env.FINTWIND_OPENCODE_SERVER_PORT = "1"; process.env.OPENCODE_SERVER_PASSWORD = "fixture";
    await plugin.setup({ session: { hook: register }, shell: { hook: register }, tool: { hook: register }, rpc: { register: async () => ({ dispose: async () => {} }) } });
    const event = { sessionID: "restarted", request: new Request("http://127.0.0.1/v1/responses", { method: "POST", body: JSON.stringify({ input: [{ role: "user", content: ${JSON.stringify(`password="${original}"`)} }] }) }) };
    await hooks.get("http.request")(event);
    console.log((await event.request.json()).input[0].content);
  `], {
    cwd: process.cwd(), env: { FINTWIND_PROMPT_PRIVACY_KEY_PATH: keyPath, SYSTEMROOT: process.env.SYSTEMROOT ?? "" },
    stdin: "ignore", stdout: "pipe", stderr: "pipe", timeout: 5000,
  });
  assert.equal(child.exitCode, 0, child.stderr.toString());
  assert.equal(child.stdout.toString().trim(), first.input[0].content, "reference changed after process restart");

  async function runCommand() {
    const event = { command: `-e 'process.stdout.write(process.env["${ref}"] ?? "")'`, env: {} };
    await hooks.get("create.before")!(event);
    // Run a real child with only this command's environment. Never print its output.
    const child = Bun.spawnSync([process.execPath, "-e", `process.stdout.write(process.env["${ref}"] ?? "")`], {
      env: event.env, stdin: "ignore", stdout: "pipe", stderr: "pipe", timeout: 3000,
    });
    assert.equal(child.exitCode, 0);
    assert.ok(child.stdout.equals(Buffer.from(original)), "shell did not receive the original bytes");
  }

  await runCommand();
  await dispose();
  dispose = await plugin.setup(context);

  const execute = hooks.get("execute.before")!;
  const originalInput = { path: ".env", content: `PASSWORD=${ref}`, options: [{ authorization: ref }] };
  const write = { sessionID, tool: "write", input: originalInput };
  await execute(write);
  assert.ok(write.input.content === `PASSWORD=${original}` && write.input.options[0].authorization === original,
    "file tool did not receive the original values");
  assert.equal(originalInput.content, `PASSWORD=${ref}`);
  const directory = await mkdtemp(join(tmpdir(), "fintwind-privacy-"));
  const outputPath = join(directory, write.input.path);
  try {
    await writeFile(outputPath, write.input.content);
    assert.ok((await readFile(outputPath, "utf8")) === `PASSWORD=${original}`, "file bytes did not match");
  } finally {
    try { await unlink(outputPath); } finally { await rmdir(directory); }
  }

  const external = { sessionID, tool: "fixture_external", input: { authorization: ref } };
  await execute(external);
  const outbound = await fetch(provider.url, {
    method: "POST", body: JSON.stringify(external.input), signal: AbortSignal.timeout(3000),
  });
  assert.ok((await outbound.json()).authorization === original, "external receiver did not receive the original value");

  for (const tool of ["shell", "execute", "subagent"]) {
    const event = { sessionID, tool, input: { content: ref } };
    await execute(event);
    assert.equal(event.input.content, ref);
  }
  const missing = "FINTWIND_SECRET_" + "0".repeat(24);
  const resolveDisplay = hooks.get("output.resolve")!;
  const display = await resolveDisplay({ sessionID, references: [ref, missing] });
  assert.ok((display as Record<string, string>)[ref] === original, "display did not receive the original value");
  assert.ok(!Object.hasOwn(display as object, missing));
  assert.deepEqual(await resolveDisplay({ sessionID: "unseen-conversation", references: [ref] }), {});
  await assert.rejects(async () => execute({ sessionID, tool: "write", input: { content: missing } }),
    /Private-value reference is unavailable/);
  await assert.rejects(async () => execute({ sessionID: "unseen-conversation", tool: "write", input: { content: ref } }),
    /Private-value reference is unavailable/);
  await assert.rejects(() => send("http", { input: [{ role: "assistant", content: `old reference ${ref}` }] }, "lost-mapping"),
    /reference.*unavailable/i, "an old reference without an original must not be sent as if it were restorable");

  for (const transport of ["http", "ws"] as const) {
    // The earlier user turn is no longer in context; only the old reference survives.
    const next = await send(transport, { input: [
      { role: "assistant", content: `Use password="${ref}" from the earlier turn.` },
      { role: "user", content: "Use the same credential again." },
    ] }, sessionID);
    assert.equal(next.input[0].content, `Use password="${ref}" from the earlier turn.`);
    await runCommand();

    const result = await send(transport, { input: [
      { type: "function_call_output", call_id: "call_reuse", output: `fixture output: ${original}` },
    ] }, sessionID);
    assert.ok(result.input[0].output === `fixture output: ${ref}`, "tool output did not reuse the original reference");
  }
  assert.equal((await send("http", { input: [{ role: "user", content: `password="${original}"` }] }, sessionID)).input[0].content,
    first.input[0].content);
  const unrelated = { command: "fixture unrelated command", env: {} };
  await hooks.get("create.before")!(unrelated);
  assert.deepEqual(unrelated.env, {});
  console.log("reuse: stable reference across turns and plugin reload; three children received exact bytes");
  console.log("tool delivery: file and external inputs restored; executable source and model requests retained references");
}

async function checkDetection() {
  const sessionID = "detection";
  const resolve = hooks.get("output.resolve")!;
  const b64url = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url");
  const signature = "Zm9vYmFyU2lnbmF0dXJlRml4dHVyZTAxMjM0NTY3ODk";
  const payload = b64url({ sub: "fixture" });
  const jwt = [b64url({ alg: "HS256", typ: "JWT" }), payload, signature].join(".");
  const fakeJwt = [b64url({ typ: "JWT" }), payload, signature].join(".");
  const refPattern = /FINTWIND_SECRET_[A-F0-9]{24}/;

  const tokenized = await send("http", { input: [{ role: "user", content: `The session token is ${jwt}` }] }, sessionID);
  const jwtRef = refPattern.exec(tokenized.input[0].content)?.[0];
  assert.ok(jwtRef, "valid JWT was not replaced");
  assert.deepEqual(await resolve({ sessionID, references: [jwtRef] }), { [jwtRef]: jwt });

  const untouched = await send("http", { input: [{ role: "user", content: `The session token is ${fakeJwt}` }] }, sessionID);
  assert.equal(untouched.input[0].content, `The session token is ${fakeJwt}`);

  // providerTokens caps sk- values at 64 characters; the plus sign sits outside
  // its lookahead, so only boundary normalization recovers the full value.
  const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz0123456789";
  const random = (length: number) =>
    Array.from(crypto.getRandomValues(new Uint8Array(length)), byte => alphabet[byte % alphabet.length]).join("");
  const longKey = ["sk-", random(64), "+", random(2)].join("");
  assert.equal(longKey.length, 70);
  const longOut = await send("http", { input: [{ role: "user", content: `Deploy with ${longKey} now.` }] }, sessionID);
  const longRef = refPattern.exec(longOut.input[0].content)?.[0];
  assert.ok(longRef, "70-character sk- value was not replaced");
  const longResolved = (await resolve({ sessionID, references: [longRef] })) as Record<string, string>;
  assert.equal(longResolved[longRef]?.length, 70);
  assert.equal(longResolved[longRef], longKey);

  const uuid = ["123e4567", "e89b", "12d3", "a456", "426614174000"].join("-");
  const identifier = await send("http", { input: [{ role: "user", content: `api_key: ${uuid}` }] }, sessionID);
  assert.equal(identifier.input[0].content, `api_key: ${uuid}`);
  const secret = await send("http", { input: [{ role: "user", content: `password=${uuid}` }] }, sessionID);
  assert.match(secret.input[0].content, /^password=FINTWIND_SECRET_[A-F0-9]{24}$/);

  let mixed = random(24);
  while (!/[A-Z]/.test(mixed) || !/[a-z]/.test(mixed)) mixed = random(24);
  const mixedOut = await send("http", { input: [{ role: "user", content: `api_key: ${mixed}` }] }, sessionID);
  const mixedRef = refPattern.exec(mixedOut.input[0].content)?.[0];
  assert.ok(mixedRef, "mixed-case api_key value was not replaced");
  assert.deepEqual(await resolve({ sessionID, references: [mixedRef] }), { [mixedRef]: mixed });

  console.log("detection: JWT structure, 70-character boundary normalization, UUID exemption and mixed-case scoring checked");
  console.log("reproduce: bun scripts/prompt-privacy-e2e.ts");
}

async function checkToolDescriptions() {
  const original = `tool-description-${crypto.randomUUID()}`;
  const output = await send("http", { input: [{ role: "user", content: "run the tool" }], tools: [{
    type: "function", name: "fixture_tool", description: `password="${original}"`,
    parameters: { type: "object", properties: { note: { type: "string", description: `password="${original}"` } } },
  }], text: { format: { type: "json_schema", name: "output", schema: { type: "object",
    properties: { note: { type: "string", description: `password="${original}"` } } } } } }, "tool-description");
  assert.ok(!JSON.stringify(output).includes(original), "tool definitions must not leak detected originals");
  assert.equal(output.tools[0].name, "fixture_tool");
  await assert.rejects(() => send("http", { input: [{ role: "user", content: `password="${original}"` }],
    tools: [{ type: "function", name: original, description: "fixture", parameters: { type: "object" } }] },
  "sensitive-tool-name"), /structure.*privacy/i, "private values in protocol identifiers must not be sent unchanged");
  await assert.rejects(() => send("http", { input: [{ role: "user", content: `password="${original}"` }],
    tools: [{ type: "function", name: `tool-${original}`, description: "fixture", parameters: { type: "object" } }] },
  "sensitive-tool-name-embedded"), /structure.*privacy/i, "embedded private values in tool names must not leak");
  await assert.rejects(() => hooks.get("http.request")!({ sessionID: "unsupported", request: new Request(
    "http://127.0.0.1/v1/unknown", { method: "POST", body: JSON.stringify({ prompt: `password="${original}"` }) },
  ) }), /unsupported.*privacy/i, "unsupported model protocols must not bypass privacy");
  await assert.rejects(() => hooks.get("http.request")!({ sessionID: "unsupported-body", request: new Request(
    "http://127.0.0.1/v1/responses", { method: "POST", body: JSON.stringify({ prompt: `password="${original}"` }) },
  ) }), /unsupported.*privacy/i, "unsupported request shape must not bypass privacy");
  const compact = { sessionID: "compact", request: new Request("http://127.0.0.1/v1/responses/compact", {
    method: "POST", body: JSON.stringify({ input: [{ role: "user", content: `password="${original}"` }] }),
  }) };
  await hooks.get("http.request")!(compact);
  assert.ok(!(await compact.request.text()).includes(original), "compaction input must be masked");
  await assert.rejects(() => hooks.get("experimental.ws.send")!({ sessionID: "unsupported-ws",
    frame: JSON.stringify({ type: "unknown.request", prompt: `password="${original}"` }) }), /unsupported.*privacy/i,
  "unsupported WebSocket protocols must not bypass privacy");
  const sessionID = "ws-continuation";
  await send("ws", { input: [{ role: "user", content: `password="${original}"` }] }, sessionID);
  const fresh = `new-secret-${crypto.randomUUID()}`;
  await assert.rejects(() => hooks.get("experimental.ws.send")!({ sessionID,
    frame: JSON.stringify({ type: "response.create", previous_response_id: "resp_old",
      input: [{ role: "user", content: `password="${fresh}"` }] }) }), /continuation.*privacy/i,
  "newly discovered values cannot safely reuse provider-side history");
  await assert.rejects(() => hooks.get("experimental.ws.send")!({ sessionID,
    frame: JSON.stringify({ type: "response.create", previous_response_id: "resp_old",
      input: [{ role: "user", content: `password="${fresh}"` }] }) }), /continuation.*privacy/i,
  "retry must not reuse the unsafe continuation");
  await assert.rejects(() => hooks.get("experimental.ws.send")!({ sessionID: "ws-unknown-cache",
    frame: JSON.stringify({ type: "response.create", previous_response_id: "resp_unknown",
      input: [{ role: "user", content: "continue" }] }) }), /continuation.*privacy/i,
  "a continuation with no local history must not assume cached text was protected");
  await assert.rejects(() => hooks.get("http.request")!({ sessionID: "http-unknown-cache",
    request: new Request("http://127.0.0.1/v1/responses", { method: "POST", body: JSON.stringify({
      previous_response_id: "resp_unknown", input: [{ role: "user", content: "continue" }],
    }) }) }), /continuation.*privacy/i, "HTTP continuations must not reuse unknown cached history");
}

async function checkLateDiscovery() {
  for (const transport of ["http", "ws"] as const) {
    const resumed = await send(transport, { input: [{ role: "assistant", content: `summary ${historicalValue}` }] }, `history-pages-${transport}`);
    assert.ok(!JSON.stringify(resumed).includes(historicalValue), "historical values from later pages must be masked");
    await assert.rejects(() => send(transport, { input: [{ role: "user", content: "resume" }] }, `history-failure-${transport}`),
      /local privacy history/, "failed history reads must block sending");
  }
  await assert.rejects(() => send("http", { input: [{ role: "user", content: "resume" }] }, "history-partial"), /local privacy history/);
  failLaterPage = false;
  const retried = await send("http", { input: [{ role: "assistant", content: `summary ${historicalValue}` }] }, "history-partial");
  assert.ok(!JSON.stringify(retried).includes(historicalValue), "retry must compile values found before a failed page");
  const sessionID = "late-discovery";
  const value = `late-${crypto.randomUUID()}`;
  const first = await send("http", { input: [{ role: "assistant", content: `earlier value ${value}` }] }, sessionID);
  assert.equal(first.input[0].content, `earlier value ${value}`);
  const next = await send("http", { input: [
    { role: "assistant", content: `earlier value ${value}` },
    { role: "user", content: `password="${value}"` },
  ] }, sessionID);
  assert.ok(!JSON.stringify(next).includes(value), "newly detected value must also be redacted in earlier history");
  console.log("history: newly discovered values override cache prefix reuse");
}

async function checkHistoryPerformance() {
  const started = performance.now();
  const first = await send("http", { input: [{ role: "assistant", content: `summary ${historicalValue}` }] }, "history-benchmark");
  assert.ok(!JSON.stringify(first).includes(historicalValue));
  const elapsed = performance.now() - started;
  const path = "/api/session/history-benchmark/message";
  assert.equal(historyReads.get(path), 50);
  await send("http", { input: [{ role: "user", content: "continue" }] }, "history-benchmark");
  assert.equal(historyReads.get(path), 50, "subsequent requests must not rescan history");
  console.log(`history performance: 10000 messages / 50 pages scanned once in ${elapsed.toFixed(1)} ms; follow-up reads 0 pages`);
}

async function checkConcurrentKeyCreation() {
  const directory = await mkdtemp(join(tmpdir(), "fintwind-privacy-race-"));
  const path = join(directory, "key");
  const source = `
    import plugin from "./resources/opencode-prompt-privacy-plugin.ts";
    const hooks = new Map();
    const register = async (name, fn) => { hooks.set(name, fn); return { dispose: async () => {} }; };
    globalThis.fetch = async () => Response.json({ data: [], cursor: { next: null } });
    process.env.FINTWIND_OPENCODE_SERVER_PORT = "1"; process.env.OPENCODE_SERVER_PASSWORD = "fixture";
    await plugin.setup({ session: { hook: register }, shell: { hook: register }, tool: { hook: register }, rpc: { register: async () => ({ dispose: async () => {} }) } });
    const event = { sessionID: "race", request: new Request("http://127.0.0.1/v1/responses", { method: "POST", body: JSON.stringify({ input: [{ role: "user", content: 'password="fixture-race-credential"' }] }) }) };
    await hooks.get("http.request")(event);
    console.log((await event.request.json()).input[0].content);
  `;
  try {
    const children = Array.from({ length: 4 }, () => Bun.spawn([process.execPath, "-e", source], {
      cwd: process.cwd(), env: { FINTWIND_PROMPT_PRIVACY_KEY_PATH: path, SYSTEMROOT: process.env.SYSTEMROOT ?? "" },
      stdin: "ignore", stdout: "pipe", stderr: "pipe", timeout: 5000,
    }));
    const results = await Promise.all(children.map(async child => ({
      exit: await child.exited,
      stdout: await new Response(child.stdout).text(),
      stderr: await new Response(child.stderr).text(),
    })));
    for (const result of results) assert.equal(result.exit, 0, result.stderr);
    assert.equal(new Set(results.map(result => result.stdout.trim())).size, 1, "concurrent processes used different keys");
    assert.match(results[0].stdout, /FINTWIND_SECRET_[A-F0-9]{24}/);
    assert.equal((await readFile(path)).length, 32);
    console.log("key: concurrent first startup uses one complete persistent key");
  } finally {
    await unlink(path);
    await rmdir(directory);
  }
}

async function checkResponsesHttp() {
  const sessionID = "responses-stream";
  const original = `stream-${crypto.randomUUID()}`;
  const sent = await send("http", { input: [{ role: "user", content: `password="${original}"` }] }, sessionID);
  const ref = /FINTWIND_SECRET_[A-F0-9]{24}/.exec(sent.input[0].content)?.[0];
  assert.ok(ref);
  const events = [
    { type: "response.output_text.delta", item_id: "msg_1", content_index: 0, delta: `hello ${ref.slice(0, 12)}` },
    { type: "response.output_text.delta", item_id: "msg_1", content_index: 0, delta: `${ref.slice(12)}!` },
    { type: "response.output_text.done", item_id: "msg_1", content_index: 0, text: `hello ${ref}!` },
    { type: "response.function_call_arguments.delta", item_id: "call_1", delta: `{"value":"${ref.slice(0, 10)}` },
    { type: "response.function_call_arguments.delta", item_id: "call_1", delta: `${ref.slice(10)}"}` },
    { type: "response.function_call_arguments.done", item_id: "call_1", arguments: `{"value":"${ref}"}` },
    { type: "response.output_item.done", item: { id: "call_1", type: "function_call", name: "write", arguments: `{"value":"${ref}"}` } },
    { type: "response.reasoning_summary_text.delta", item_id: "reason_1", summary_index: 0, delta: ref.slice(0, 8) },
    { type: "response.reasoning_summary_text.delta", item_id: "reason_1", summary_index: 0, delta: ref.slice(8) },
    { type: "response.reasoning_summary_text.done", item_id: "reason_1", summary_index: 0, text: ref },
    { type: "response.completed", response: { id: "resp_fixture", output: [
      { type: "function_call", id: "call_1", call_id: "call_1", name: "write", arguments: `{"value":"${ref}"}` },
    ] } },
  ];
  const body = events.map(value => `event: ${value.type}\ndata: ${JSON.stringify(value)}\n\n`).join("") + "data: [DONE]\n\n";
  const encoded = new TextEncoder().encode(body);
  const chunks = new ReadableStream<Uint8Array>({
    start(controller) {
      for (let offset = 0; offset < encoded.length; offset += 7) controller.enqueue(encoded.slice(offset, offset + 7));
      controller.close();
    },
  });
  const event = { sessionID, request: new Request("http://127.0.0.1/v1/responses"),
    response: new Response(chunks, { headers: { "content-type": "text/event-stream" } }) };
  const hook = hooks.get("http.response");
  assert.ok(hook, "HTTP response hook must be registered");
  await hook(event);
  const result = await event.response.text();
  const decoded = [...result.matchAll(/^data: (.+)$/gm)].map(match => match[1])
    .filter(text => text !== "[DONE]").map(text => JSON.parse(text));
  assert.equal(decoded.filter(item => item.type === "response.output_text.delta").map(item => item.delta).join(""), `hello ${original}!`);
  assert.equal(decoded.find(item => item.type === "response.output_text.done").text, `hello ${original}!`);
  assert.equal(decoded.filter(item => item.type === "response.function_call_arguments.delta").map(item => item.delta).join(""), "");
  assert.equal(JSON.parse(decoded.find(item => item.type === "response.function_call_arguments.done").arguments).value, original);
  assert.equal(JSON.parse(decoded.find(item => item.type === "response.output_item.done").item.arguments).value, original);
  assert.equal(decoded.filter(item => item.type === "response.reasoning_summary_text.delta").map(item => item.delta).join(""), original);
  assert.equal(decoded.find(item => item.type === "response.completed").response.id, "resp_fixture");
  assert.equal(JSON.parse(decoded.find(item => item.type === "response.completed").response.output[0].arguments).value, original);
  const snapshot = { sessionID, request: new Request("http://127.0.0.1/v1/responses"), response: new Response(
    `event: response.content_part.done\ndata: ${JSON.stringify({ type: "response.content_part.done", item_id: "msg1", content_index: 0,
      part: { type: "output_text", text: ref } })}\n\n`, { headers: { "content-type": "text/event-stream" } }) };
  await hooks.get("http.response")!(snapshot);
  assert.equal(JSON.parse((await snapshot.response.text()).split("data: ")[1]).part.text, original);
  const embedded = { sessionID, request: new Request("http://127.0.0.1/v1/responses"), response: Response.json({
    output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: `prefix_${ref}_suffix` }] }],
  }) };
  await hooks.get("http.response")!(embedded);
  assert.equal((await embedded.response.json()).output[0].content[0].text, `prefix_${original}_suffix`);
  const next = await send("http", { input: [{ role: "assistant", content: `hello ${original}!` }] }, sessionID);
  assert.equal(next.input[0].content, `hello ${ref}!`);
  const refusalEvent = { sessionID, request: new Request("http://127.0.0.1/v1/responses"), response: new Response(
    [{ type: "response.refusal.delta", item_id: "refusal_1", content_index: 0, delta: ref.slice(0, 11) },
     { type: "response.refusal.delta", item_id: "refusal_1", content_index: 0, delta: ref.slice(11) },
     { type: "response.refusal.done", item_id: "refusal_1", content_index: 0, refusal: ref }]
      .map(item => `event: ${item.type}\ndata: ${JSON.stringify(item)}\n\n`).join(""),
    { headers: { "content-type": "text/event-stream" } }) };
  await hooks.get("http.response")!(refusalEvent);
  const refusal = [...(await refusalEvent.response.text()).matchAll(/^data: (.+)$/gm)].map(match => JSON.parse(match[1]));
  assert.equal(refusal.filter(item => item.type === "response.refusal.delta").map(item => item.delta).join(""), original);
  assert.equal(refusal.find(item => item.type === "response.refusal.done").refusal, original);
  console.log("responses HTTP: split network chunks and stream events restored and redacted on return");
}

async function checkResponsesWebSocket() {
  const sessionID = "responses-ws-stream";
  const original = `ws-stream-${crypto.randomUUID()}`;
  const sent = await send("ws", { input: [{ role: "user", content: `password="${original}"` }] }, sessionID);
  const ref = /FINTWIND_SECRET_[A-F0-9]{24}/.exec(sent.input[0].content)?.[0];
  assert.ok(ref);
  const receive = hooks.get("experimental.ws.receive");
  assert.ok(receive, "WebSocket receive hook must be registered");
  const frame = async (value: Record<string, unknown>) => {
    const event = { sessionID, frame: JSON.stringify(value) };
    await receive(event);
    return JSON.parse(event.frame);
  };
  const first = await frame({ type: "response.output_text.delta", item_id: "msg_1", content_index: 0, delta: `value ${ref.slice(0, 13)}` });
  const second = await frame({ type: "response.output_text.delta", item_id: "msg_1", content_index: 0, delta: ref.slice(13) });
  assert.equal(first.delta + second.delta, `value ${original}`);
  const done = await frame({ type: "response.output_text.done", item_id: "msg_1", content_index: 0, text: `value ${ref}` });
  assert.equal(done.text, `value ${original}`);
  const completed = await frame({ type: "response.completed", response: { id: "r1", output: [{
    type: "function_call", id: "call1", call_id: "call1", name: "write", arguments: JSON.stringify({ value: ref }),
  }] } });
  assert.equal(JSON.parse(completed.response.output[0].arguments).value, original);
  const missing = "FINTWIND_SECRET_" + "0".repeat(24);
  await frame({ type: "response.output_text.delta", item_id: "msg_missing", content_index: 0, delta: missing.slice(0, 15) });
  await assert.rejects(() => frame({ type: "response.output_text.delta", item_id: "msg_missing", content_index: 0,
    delta: missing.slice(15) }), /reference.*unavailable/i, "unknown reference must not reach OpenCode");
  await assert.rejects(() => frame({ type: "response.output_item.done", item: { type: "function_call", id: "missing_call",
    call_id: "missing_call", name: "write", arguments: JSON.stringify({ value: missing }) } }), /reference.*unavailable/i,
    "unknown reference in final tool input must not reach OpenCode");
  console.log("responses WebSocket: fragmented references restored in original event order");
}

async function checkChatHttp() {
  const sessionID = "chat-stream";
  const original = `chat-${crypto.randomUUID()}`;
  const sent = await send("http", { messages: [{ role: "user", content: `password="${original}"` }] }, sessionID);
  const ref = /FINTWIND_SECRET_[A-F0-9]{24}/.exec(sent.messages[0].content)?.[0];
  assert.ok(ref);
  const events = [
    { id: "chat-fixture", choices: [{ index: 0, delta: { content: `value ${ref.slice(0, 11)}` }, finish_reason: null }] },
    { id: "chat-fixture", choices: [{ index: 0, delta: { content: ref.slice(11) }, finish_reason: null }] },
    { id: "chat-fixture", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] },
  ];
  const event = { sessionID, request: new Request("http://127.0.0.1/v1/chat/completions"), response: new Response(
    events.map(item => `data: ${JSON.stringify(item)}\n\n`).join("") + "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  ) };
  await hooks.get("http.response")!(event);
  const result = await event.response.text();
  const decoded = [...result.matchAll(/^data: (.+)$/gm)].map(match => match[1])
    .filter(text => text !== "[DONE]").map(text => JSON.parse(text));
  assert.equal(decoded.flatMap(item => item.choices ?? []).map(choice => choice.delta?.content ?? "").join(""), `value ${original}`);
  const nonstream = { sessionID, request: new Request("http://127.0.0.1/v1/chat/completions"), response: Response.json({
    id: "chat-fixture", choices: [{ index: 0, message: { role: "assistant", content: `value ${ref}` }, finish_reason: "stop" }],
  }) };
  await hooks.get("http.response")!(nonstream);
  assert.equal((await nonstream.response.json()).choices[0].message.content, `value ${original}`);
  const originalJson = '{ "id": 123456789012345678901234567890, "choices": [] }';
  const unchanged = { sessionID, request: new Request("http://127.0.0.1/v1/chat/completions"), response: new Response(originalJson,
    { headers: { "content-type": "application/json" } }) };
  await hooks.get("http.response")!(unchanged);
  assert.equal(await unchanged.response.text(), originalJson, "unmodified JSON responses must retain the exact bytes");
  const finishWithDelta = { sessionID, request: new Request("http://127.0.0.1/v1/chat/completions"), response: new Response(
    [ { choices: [{ index: 0, delta: { content: `x ${ref.slice(0, 10)}` }, finish_reason: null }] },
      { choices: [{ index: 0, delta: { content: ref.slice(10) }, finish_reason: "stop" }] } ]
      .map(item => `data: ${JSON.stringify(item)}\n\n`).join(""), { headers: { "content-type": "text/event-stream" } }) };
  await hooks.get("http.response")!(finishWithDelta);
  const finished = [...(await finishWithDelta.response.text()).matchAll(/^data: (.+)$/gm)].map(match => JSON.parse(match[1]));
  assert.equal(finished.flatMap(item => item.choices ?? []).map(choice => choice.delta?.content ?? "").join(""), `x ${original}`);
  console.log("chat HTTP: fragmented assistant text restored before parsing");
}

async function checkAnthropicHttp() {
  const sessionID = "anthropic-stream";
  const original = `anthropic-${crypto.randomUUID()}`;
  const sent = await send("http", { messages: [{ role: "user", content: `password="${original}"` }] }, sessionID);
  const ref = /FINTWIND_SECRET_[A-F0-9]{24}/.exec(sent.messages[0].content)?.[0];
  assert.ok(ref);
  const events = [
    { type: "content_block_start", index: 0, content_block: { type: "thinking", thinking: "" } },
    { type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: ref } },
    { type: "content_block_delta", index: 0, delta: { type: "signature_delta", signature: "fixture-signature" } },
    { type: "content_block_stop", index: 0 },
    { type: "content_block_start", index: 1, content_block: { type: "text", text: "" } },
    { type: "content_block_delta", index: 1, delta: { type: "text_delta", text: ref.slice(0, 10) } },
    { type: "content_block_delta", index: 1, delta: { type: "text_delta", text: ref.slice(10) } },
    { type: "content_block_stop", index: 1 },
    { type: "content_block_start", index: 2, content_block: { type: "tool_use", id: "tool1", name: "write", input: {} } },
    { type: "content_block_delta", index: 2, delta: { type: "input_json_delta", partial_json: `{"value":"${ref.slice(0, 12)}` } },
    { type: "content_block_delta", index: 2, delta: { type: "input_json_delta", partial_json: `${ref.slice(12)}"}` } },
    { type: "content_block_stop", index: 2 },
    { type: "message_stop" },
  ];
  const event = { sessionID, request: new Request("http://127.0.0.1/v1/messages"), response: new Response(
    events.map(item => `event: ${item.type}\ndata: ${JSON.stringify(item)}\n\n`).join(""),
    { headers: { "content-type": "text/event-stream" } },
  ) };
  await hooks.get("http.response")!(event);
  const decoded = [...(await event.response.text()).matchAll(/^data: (.+)$/gm)].map(match => JSON.parse(match[1]));
  assert.equal(decoded.filter(item => item.delta?.type === "text_delta").map(item => item.delta.text).join(""), original);
  assert.equal(JSON.parse(decoded.filter(item => item.delta?.type === "input_json_delta").map(item => item.delta.partial_json).join("")).value, original);
  assert.equal(decoded.filter(item => item.delta?.type === "thinking_delta").map(item => item.delta.thinking).join(""), original);
  assert.equal(decoded.find(item => item.delta?.type === "signature_delta").delta.signature, "fixture-signature");
  const replay = await send("http", { messages: [{ role: "assistant", content: [
    { type: "thinking", thinking: original, signature: "fixture-signature" },
  ] }] }, sessionID);
  assert.equal(replay.messages[0].content[0].thinking, ref);
  console.log("anthropic HTTP: text and tools restored; signed thinking preserved");
}

async function checkGeminiHttp() {
  const sessionID = "gemini-stream";
  const original = `gemini-${crypto.randomUUID()}`;
  const sent = await send("http", { contents: [{ role: "user", parts: [{ text: `password="${original}"` }] }] }, sessionID);
  const ref = /FINTWIND_SECRET_[A-F0-9]{24}/.exec(sent.contents[0].parts[0].text)?.[0];
  assert.ok(ref);
  const events = [
    { candidates: [{ index: 0, content: { role: "model", parts: [{ text: ref.slice(0, 12) }] } }] },
    { candidates: [{ index: 0, content: { role: "model", parts: [{ text: ref.slice(12) }] } }] },
    { candidates: [{ index: 1, content: { role: "model", parts: [{ text: "ordinary FINTWIND_" }] }, finishReason: "STOP" }] },
    { candidates: [{ index: 0, content: { role: "model", parts: [{ functionCall: { name: "write", args: { value: ref } }, thoughtSignature: "opaque-signature" }] }, finishReason: "STOP" }] },
  ];
  const event = { sessionID, request: new Request("http://127.0.0.1/v1beta/models/fixture:streamGenerateContent?alt=sse"),
    response: new Response(events.map(item => `data: ${JSON.stringify(item)}\n\n`).join(""),
      { headers: { "content-type": "text/event-stream" } }) };
  await hooks.get("http.response")!(event);
  const decoded = [...(await event.response.text()).matchAll(/^data: (.+)$/gm)].map(match => JSON.parse(match[1]));
  const parts = decoded.flatMap(item => item.candidates ?? []).flatMap(candidate => candidate.content?.parts ?? []);
  const mainParts = decoded.flatMap(item => item.candidates ?? []).filter(candidate => candidate.index === 0)
    .flatMap(candidate => candidate.content?.parts ?? []);
  assert.equal(mainParts.map(part => part.text ?? "").join(""), original);
  const trailing = decoded.flatMap(item => item.candidates ?? []).filter(candidate => candidate.index === 1)
    .flatMap(candidate => candidate.content?.parts ?? []).map(part => part.text ?? "").join("");
  assert.equal(trailing, "ordinary FINTWIND_");
  assert.equal(parts.find(part => part.functionCall).functionCall.args.value, original);
  assert.equal(parts.find(part => part.functionCall).thoughtSignature, "opaque-signature");
  console.log("gemini HTTP: text and function input restored; opaque signature retained");
}

async function checkSignedRoundTrip() {
  const sessionID = "signed-round-trip";
  const original = `signed-${crypto.randomUUID()}`;
  const sent = await send("http", { messages: [{ role: "user", content: `password="${original}"` }] }, sessionID);
  const ref = /FINTWIND_SECRET_[A-F0-9]{24}/.exec(sent.messages[0].content)?.[0];
  assert.ok(ref);
  const signed = { type: "thinking", thinking: `reason ${ref}`, signature: "fixture-signature" };
  const incoming = { sessionID, request: new Request("http://127.0.0.1/v1/messages"),
    response: Response.json({ type: "message", content: [signed] }) };
  await hooks.get("http.response")!(incoming);
  const restored = (await incoming.response.json()).content[0];
  assert.equal(restored.thinking, `reason ${original}`);
  const outgoing = await send("http", { messages: [{ role: "assistant", content: [restored] }] }, sessionID);
  assert.deepEqual(outgoing.messages[0].content[0], signed, "signed content must exactly match the original model payload");
  const leaked = `newly-found-${crypto.randomUUID()}`;
  const leakedSigned = { type: "thinking", thinking: `reason ${ref} password="${leaked}"`, signature: "leaked-signature" };
  const leakedIncoming = { sessionID, request: new Request("http://127.0.0.1/v1/messages"),
    response: Response.json({ type: "message", content: [leakedSigned] }) };
  await hooks.get("http.response")!(leakedIncoming);
  const leakedRestored = (await leakedIncoming.response.json()).content[0];
  await assert.rejects(() => send("http", { messages: [
    { role: "user", content: `password="${leaked}"` },
    { role: "assistant", content: [leakedRestored] },
  ] }, sessionID), /signed thinking/i, "newly discovered originals in old signed thinking must not leak");
  const child = Bun.spawnSync([process.execPath, "-e", `
    import plugin from "./resources/opencode-prompt-privacy-plugin.ts";
    const hooks = new Map();
    const register = async (name, fn) => { hooks.set(name, fn); return { dispose: async () => {} }; };
    globalThis.fetch = async () => Response.json({ data: [], cursor: { next: null } });
    process.env.FINTWIND_OPENCODE_SERVER_PORT = "1"; process.env.OPENCODE_SERVER_PASSWORD = "fixture";
    await plugin.setup({ session: { hook: register }, shell: { hook: register }, tool: { hook: register }, rpc: { register: async () => ({ dispose: async () => {} }) } });
    const event = { sessionID: "restart", request: new Request("http://127.0.0.1/v1/messages", { method: "POST", body: JSON.stringify({ messages: [
      { role: "user", content: ${JSON.stringify(`password="${original}"`)} },
      { role: "assistant", content: [${JSON.stringify(restored)}] },
    ] }) }) };
    try { await hooks.get("http.request")(event); process.exit(2); }
    catch (error) { if (!String(error).includes("signed thinking")) throw error; }
  `], { cwd: process.cwd(), env: { FINTWIND_PROMPT_PRIVACY_KEY_PATH: keyPath, SYSTEMROOT: process.env.SYSTEMROOT ?? "" },
    stdin: "ignore", stdout: "pipe", stderr: "pipe", timeout: 5000 });
  assert.equal(child.exitCode, 0, child.stderr.toString());
  console.log("anthropic signed thinking: restored locally and recreated byte-for-byte for model");
}

async function checkNestedJsonArguments() {
  const sessionID = "nested-json-stream";
  const original = "nested\"\\fixture-" + crypto.randomUUID();
  const sent = await send("http", { input: [{ type: "function_call_output", call_id: "fixture",
    output: JSON.stringify({ password: original }) }] }, sessionID);
  const ref = JSON.parse(sent.input[0].output).password;
  assert.match(ref, /^FINTWIND_SECRET_[A-F0-9]{24}$/);
  const argumentsJson = `{"count":123456789012345678901234567890,"text":${JSON.stringify(JSON.stringify({ value: ref }))}}`;
  const events = [
    { type: "response.function_call_arguments.delta", item_id: "nested", delta: argumentsJson },
    { type: "response.function_call_arguments.done", item_id: "nested", arguments: argumentsJson },
  ];
  const event = { sessionID, request: new Request("http://127.0.0.1/v1/responses"), response: new Response(
    events.map(item => `event: ${item.type}\ndata: ${JSON.stringify(item)}\n\n`).join(""),
    { headers: { "content-type": "text/event-stream" } }) };
  await hooks.get("http.response")!(event);
  const decoded = [...(await event.response.text()).matchAll(/^data: (.+)$/gm)].map(match => JSON.parse(match[1]));
  assert.equal(decoded[0].delta, "");
  assert.equal(JSON.parse(JSON.parse(decoded[1].arguments).text).value, original);
  assert.ok(decoded[1].arguments.includes("123456789012345678901234567890"), "restoring references must not round large tool input numbers");
  const receive = hooks.get("experimental.ws.receive")!;
  const wsEvent = { sessionID, frame: JSON.stringify({ type: "response.function_call_arguments.delta", item_id: "nested-ws", delta: argumentsJson }) };
  await receive(wsEvent);
  assert.equal(JSON.parse(wsEvent.frame).delta, "");
  wsEvent.frame = JSON.stringify({ type: "response.function_call_arguments.done", item_id: "nested-ws", arguments: argumentsJson });
  await receive(wsEvent);
  assert.equal(JSON.parse(JSON.parse(JSON.parse(wsEvent.frame).arguments).text).value, original);
  const chatEvents = [
    { id: "chat", choices: [{ index: 0, delta: { tool_calls: [{ index: 0, id: "call", type: "function",
      function: { name: "write", arguments: argumentsJson.slice(0, 12) } }] }, finish_reason: null }] },
    { id: "chat", choices: [{ index: 0, delta: { tool_calls: [{ index: 0, function: { arguments: argumentsJson.slice(12) } }] }, finish_reason: "tool_calls" }] },
  ];
  const chatEvent = { sessionID, request: new Request("http://127.0.0.1/v1/chat/completions"), response: new Response(
    chatEvents.map(item => `data: ${JSON.stringify(item)}\n\n`).join(""), { headers: { "content-type": "text/event-stream" } }) };
  await hooks.get("http.response")!(chatEvent);
  const chatDecoded = [...(await chatEvent.response.text()).matchAll(/^data: (.+)$/gm)].map(match => JSON.parse(match[1]));
  const joined = chatDecoded.flatMap(item => item.choices).flatMap(choice => choice.delta.tool_calls ?? [])
    .map(tool => tool.function?.arguments ?? "").join("");
  assert.equal(JSON.parse(JSON.parse(joined).text).value, original);
  const anthEvents = [
    { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: "call", name: "write", input: {} } },
    { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: argumentsJson.slice(0, 12) } },
    { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: argumentsJson.slice(12) } },
    { type: "content_block_stop", index: 0 },
  ];
  const anthEvent = { sessionID, request: new Request("http://127.0.0.1/v1/messages"), response: new Response(
    anthEvents.map(item => `event: ${item.type}\ndata: ${JSON.stringify(item)}\n\n`).join(""),
    { headers: { "content-type": "text/event-stream" } }) };
  await hooks.get("http.response")!(anthEvent);
  const anthDecoded = [...(await anthEvent.response.text()).matchAll(/^data: (.+)$/gm)].map(match => JSON.parse(match[1]));
  const anthArgs = anthDecoded.filter(item => item.delta?.type === "input_json_delta").map(item => item.delta.partial_json).join("");
  assert.equal(JSON.parse(JSON.parse(anthArgs).text).value, original);
  const invalid = { sessionID, request: new Request("http://127.0.0.1/v1/responses"), response: new Response(
    [{ type: "response.function_call_arguments.delta", item_id: "bad", delta: '{"value":"' + ref },
      { type: "response.output_item.done", item: { type: "function_call", id: "bad", call_id: "bad", name: "write" } }]
      .map(item => `event: ${item.type}\ndata: ${JSON.stringify(item)}\n\n`).join(""),
    { headers: { "content-type": "text/event-stream" } }) };
  await hooks.get("http.response")!(invalid);
  await assert.rejects(() => invalid.response.text(), /incomplete JSON/i, "incomplete tool input must not execute");
  const nonstream = { sessionID, request: new Request("http://127.0.0.1/v1/responses"), response: Response.json({
    output: [{ type: "function_call", id: "call_nested", arguments: argumentsJson }],
  }) };
  await hooks.get("http.response")!(nonstream);
  const rawArgs = (await nonstream.response.json()).output[0].arguments;
  assert.ok(rawArgs.includes("123456789012345678901234567890"), "non-stream JSON must not round large tool input numbers");
  const args = JSON.parse(rawArgs);
  assert.equal(JSON.parse(args.text).value, original);
  console.log("nested JSON tool input: restored after complete arguments arrive");
}

try {
  assert.ok(!hooks.has("context") && !hooks.has("compaction"), "privacy must not inject reference instructions into model context");
  await checkConcurrentKeyCreation();
  await checkReferenceReuse();
  await checkDetection();
  await checkToolDescriptions();
  await checkLateDiscovery();
  await checkHistoryPerformance();
  await checkResponsesHttp();
  await checkResponsesWebSocket();
  await checkChatHttp();
  await checkAnthropicHttp();
  await checkGeminiHttp();
  await checkSignedRoundTrip();
  await checkNestedJsonArguments();
  const failures: unknown[] = [];
  for (const transport of ["http", "ws"] as const) {
    try {
      const { input: [user] } = await send(transport, { input: [{ role: "user", content: 'password="al_ans"\npassword="menta"' }] });
      const [finalRef, commentaryRef] = user.content.match(/FINTWIND_SECRET_[A-F0-9]{24}/g) ?? [];
      assert.ok(finalRef && commentaryRef && finalRef !== commentaryRef);

      const { input: [final, commentary, tool] } = await send(transport, { input: [
        { type: "message", role: "assistant", phase: "final_answer", content: "al_ans" },
        { role: "assistant", phase: "commentary", content: "menta" },
        { type: "function_call", name: "fixture", call_id: "call_fixture", arguments: '{"phase":"al_ans"}' },
      ] });
      assert.equal(final.phase, "final_answer");
      assert.equal(commentary.phase, "commentary");
      assert.equal(final.content, finalRef);
      assert.equal(commentary.content, commentaryRef);
      assert.equal(JSON.parse(tool.arguments).phase, finalRef);
      console.log(`${transport}: assistant phases preserved; user text and tool data redacted`);

      const options = {
        service_tier: "auto",
        tool_choice: "required",
        reasoning: { effort: "medium" },
        text: { format: { type: "json_schema", name: "fixture", schema: {
          type: "object", properties: { medium: { type: "string" } }, required: ["medium"],
        } } },
      };
      const image = { type: "input_image", image_url: "data:image/png;base64,auto", detail: "auto" };
      const { input: [prompt, result, reasoning], instructions, type: _frameType, ...actualOptions } = await send(transport, {
        ...options,
        instructions: 'password="in"',
        input: [
          { role: "user", content: [image, { type: "input_text", text: 'continue installing; password="auto"\npassword="medium"\npassword="required"' }] },
          { type: "function_call_output", call_id: "call_fixture", output: '{"password":"in"}' },
          { type: "reasoning", encrypted_content: "al_ans", summary: [{ type: "summary_text", text: 'password="al_ans"' }] },
        ],
      });
      assert.deepEqual(actualOptions, options);
      assert.deepEqual(prompt.content[0], image);
      assert.match(prompt.content[1].text, /^continue installing; password="FINTWIND_SECRET_/);
      const shortRef = /^password="(FINTWIND_SECRET_[A-F0-9]{24})"$/.exec(instructions)?.[1];
      assert.ok(shortRef);
      assert.equal(JSON.parse(result.output).password, shortRef);

      const privateText = 'password="al_ans"';
      const maskedText = `password="${finalRef}"`;
      assert.equal(reasoning.encrypted_content, "al_ans");
      assert.equal(reasoning.summary[0].text, maskedText);
      const chat = await send(transport, { messages: [
        { role: "system", content: privateText },
        { role: "assistant", tool_calls: [{ type: "function", function: { name: "fixture_tool", arguments: '{"name":"al_ans"}' } }] },
        { role: "tool", content: [{ type: "text", text: privateText }] },
      ] });
      assert.equal(chat.messages[0].content, maskedText);
      assert.equal(chat.messages[1].tool_calls[0].function.name, "fixture_tool");
      assert.equal(JSON.parse(chat.messages[1].tool_calls[0].function.arguments).name, finalRef);
      assert.equal(chat.messages[2].content[0].text, maskedText);

      const signedBlock = { type: "thinking", thinking: "ordinary reasoning", signature: "al_ans" };
      await assert.rejects(() => send(transport, { messages: [{ role: "assistant", content: [signedBlock] }] }), /signed thinking/i);
      const anthropic = await send(transport, {
        system: [{ type: "text", text: privateText }],
        messages: [
          { role: "assistant", content: [{ type: "tool_use", input: { name: "al_ans" } },
            { type: "mcp_tool_result", content: [{ type: "text", text: privateText }] }] },
          { role: "user", content: [{ type: "tool_result", content: [{ type: "text", text: privateText }] }] },
        ],
      });
      assert.equal(anthropic.system[0].text, maskedText);
      assert.equal(anthropic.messages[0].content[0].input.name, finalRef);
      assert.equal(anthropic.messages[0].content[1].content[0].text, maskedText);
      assert.equal(anthropic.messages[1].content[0].content[0].text, maskedText);
      const safeSigned = { type: "thinking", thinking: `password="${finalRef}"`, signature: "signed-fixture" };
      await assert.rejects(() => send(transport, { messages: [{ role: "assistant", content: [safeSigned] }] }), /signed thinking/i,
        "a signed block without a recorded source must not be replayed");
      await assert.rejects(() => send(transport, { messages: [{ role: "assistant", content: [
        { type: "thinking", thinking: privateText, signature: "signed-fixture" },
      ] }] }), /signed thinking/i, "signed thinking containing a private value must not be sent");
      await assert.rejects(() => send(transport, { messages: [{ role: "assistant", content: [
        { type: "thinking", thinking: privateText, signature: "signed-fixture" },
      ] }] }, `${transport}-signed-new-session`), /signed thinking/i,
      "signed thinking must be scanned even without an existing mapping");

      const gemini = await send(transport, {
        systemInstruction: { parts: [{ text: privateText }] },
        contents: [{ role: "model", parts: [{ functionCall: { name: "fixture_tool", args: { name: "al_ans" } } }] },
          { role: "user", parts: [{ functionResponse: { name: "fixture_tool", response: { name: "al_ans" } } }] }],
      });
      assert.equal(gemini.systemInstruction.parts[0].text, maskedText);
      assert.deepEqual(gemini.contents[0].parts[0].functionCall, { name: "fixture_tool", args: { name: finalRef } });
      assert.deepEqual(gemini.contents[1].parts[0].functionResponse, { name: "fixture_tool", response: { name: finalRef } });
      console.log(`${transport}: request options and media preserved; short passwords and OpenAI, Anthropic, Gemini content checked`);
    } catch (error) {
      failures.push(new Error(`${transport}: request validation failed`, { cause: error }));
    }
  }
  if (failures.length) throw new AggregateError(failures);
} finally {
  provider.stop(true);
  await dispose();
  await unlink(keyPath);
  await rmdir(keyDirectory);
}
