import assert from "node:assert/strict";
import { mkdtemp, readFile, unlink, rmdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import plugin from "../resources/opencode-prompt-privacy-plugin.ts";

// Isolated fixture: no OpenCode process, real provider, or user settings.
delete process.env.FINTWIND_PROMPT_PRIVACY_SETTINGS;
const hooks = new Map<string, (event: unknown) => unknown>();
function register(name: string, callback: (event: never) => unknown) {
  hooks.set(name, callback as (event: unknown) => unknown);
  return Promise.resolve({ async dispose() { hooks.delete(name); } });
}
const context = {
  session: { hook: register }, shell: { hook: register }, tool: { hook: register },
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
    const event = { sessionID, request: new Request(provider.url, {
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
  assert.notEqual(other.input[0].content, first.input[0].content);

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
  assert.deepEqual(await resolveDisplay({ sessionID: "other-conversation", references: [ref] }), {});
  await assert.rejects(async () => execute({ sessionID, tool: "write", input: { content: missing } }),
    /Private-value reference is unavailable/);
  await assert.rejects(async () => execute({ sessionID: "other-conversation", tool: "write", input: { content: ref } }),
    /Private-value reference is unavailable/);

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

try {
  await checkReferenceReuse();
  await checkDetection();
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
        { role: "assistant", tool_calls: [{ type: "function", function: { name: "al_ans", arguments: '{"name":"al_ans"}' } }] },
        { role: "tool", content: [{ type: "text", text: privateText }] },
      ] });
      assert.equal(chat.messages[0].content, maskedText);
      assert.equal(chat.messages[1].tool_calls[0].function.name, "al_ans");
      assert.equal(JSON.parse(chat.messages[1].tool_calls[0].function.arguments).name, finalRef);
      assert.equal(chat.messages[2].content[0].text, maskedText);

      const signedBlock = { type: "thinking", thinking: "al_ans", signature: "al_ans" };
      const anthropic = await send(transport, {
        system: [{ type: "text", text: privateText }],
        messages: [
          { role: "assistant", content: [signedBlock, { type: "tool_use", input: { name: "al_ans" } },
            { type: "mcp_tool_result", content: [{ type: "text", text: privateText }] }] },
          { role: "user", content: [{ type: "tool_result", content: [{ type: "text", text: privateText }] }] },
        ],
      });
      assert.equal(anthropic.system[0].text, maskedText);
      assert.deepEqual(anthropic.messages[0].content[0], signedBlock);
      assert.equal(anthropic.messages[0].content[1].input.name, finalRef);
      assert.equal(anthropic.messages[0].content[2].content[0].text, maskedText);
      assert.equal(anthropic.messages[1].content[0].content[0].text, maskedText);

      const gemini = await send(transport, {
        systemInstruction: { parts: [{ text: privateText }] },
        contents: [{ role: "model", parts: [{ functionCall: { name: "al_ans", args: { name: "al_ans" } } }] },
          { role: "user", parts: [{ functionResponse: { name: "al_ans", response: { name: "al_ans" } } }] }],
      });
      assert.equal(gemini.systemInstruction.parts[0].text, maskedText);
      assert.deepEqual(gemini.contents[0].parts[0].functionCall, { name: "al_ans", args: { name: finalRef } });
      assert.deepEqual(gemini.contents[1].parts[0].functionResponse, { name: "al_ans", response: { name: finalRef } });
      console.log(`${transport}: request options and media preserved; short passwords and OpenAI, Anthropic, Gemini content checked`);
    } catch (error) {
      failures.push(new Error(`${transport}: request validation failed`, { cause: error }));
    }
  }
  if (failures.length) throw new AggregateError(failures);
} finally {
  provider.stop(true);
  await dispose();
}
