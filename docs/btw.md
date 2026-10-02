# Native /btw support

`/btw <question>` asks OpenCode for a one-shot answer using the current
session's settled context and selected model. It never admits a prompt, steers
or queues the main task, creates a turn, or saves the question/answer to task
history. The native right panel holds one transient answer per session.

## Usage

After starting a conversation, type `/btw <question>` in the composer and send
it with Enter, the send button, or the steering shortcut. The side answer opens
in a native `/btw` tab even if the main task is working. A new question replaces
the session's previous side answer; it does not form a separate multi-turn chat.

The panel supports Markdown selection/scrolling, copy, retry, and cancellation.
Switching sessions cancels a pending question but retains a completed answer.
Closing the tab removes its question and answer; Escape inside the panel closes
it and returns focus to the composer. No side answers survive app restart.

Empty questions and new attachments are rejected without losing the input.
After changing the model/variant, send a normal message first to apply the new
selection to OpenCode. Side generation never changes the native session's model
on its own. It still consumes a real model call when successfully submitted.

Desktop/daemon protocol version is now 11. Update both binaries together, and
update a remote daemon before connecting this desktop; older protocol versions
are refused at the handshake instead of timing out on an unknown command.

## Failure cases and acceptance criteria (before implementation)

- Enter, steering submission, and the send button must all intercept the exact
  `/btw` command before ordinary submission, including while the task is busy.
  This includes historical message editors: a side question must never rewind
  history, and rejected input/attachments stay in that same editor.
  `/btwhatever` remains ordinary input.
- Empty questions, attachments, or a task without a native session must leave
  the editable input intact and explain why no request was sent.
- The command picker offers `/btw` without relying on the server's command list.
- The daemon uses the existing authenticated private OpenCode server and only
  generates through `POST /api/session/{nativeID}/generate` with a `prompt`
  body. A read-only session lookup checks the selected model/variant first.
  It must not call `/prompt`, `/interrupt`, `/fork`, or switch the model/agent.
- If the newly selected model/variant has not been applied to the native
  session, preserve the input and explain that a normal message must apply
  the selection first. Never silently charge a side question to the old model.
  The daemon also checks this after restart or attachment to another client.
- A desktop and daemon with incompatible protocol versions must fail the
  handshake, not silently accept unsupported generation/cancel commands.
- A running main task continues emitting events during a side generation.
- Empty/invalid responses and HTTP failures surface as retryable side-panel
  errors, never as a main-task failure or an automatic duplicate request.
- Replacement, cancellation, session switch, and tab close abandon the old
  request. Its eventual response cannot overwrite the new answer or reopen a
  closed tab. Cancellation must not interrupt the main session.
- Completed answers remain session-local when switching tasks. They disappear
  on application restart and on deletion of the session or closing the tab.
- Answer Markdown preparation runs off the UI thread and uses virtualized
  native rendering. Copy, retry, cancel, and tab controls support the keyboard;
  Escape in the side panel returns to the composer without stopping the task.

## Repeatable transport E2E

Run the real client -> WebSocket daemon -> private OpenCode HTTP chain against
the local scripted provider (no paid model requests):

```sh
cargo test --locked -p fintwind-core --test opencode_recovery opencode_btw -- --ignored --nocapture
```

Each run writes `temp/recovery-e2e/<uuid>/result-btw.json` and provider request
logs. The artifact records history equality, request routing, cancellation,
errors, and continued main-task output. It is a transport/behavior check, not a
visual test or a claim of verification against a live provider.

### Verification record (2026-10-02)

- The side-generation E2E passed, including authenticated routing, model and
  variant mismatch rejection before generation, empty/invalid/HTTP-error
  responses, early cancellation, retry, and main-task output while a generation
  was still pending. Semantic transcript contents/status were unchanged.
- Artifact: `temp/recovery-e2e/018893ed08bc4d5180a5149ae7aa3830/result-btw.json`.
  Cancellation completed in 93 ms in that run (not a general latency guarantee).
- The existing recovery suite passed its three default tests; its seven
  opt-in tests were skipped (the side-generation case was run separately).
- Native visual/keyboard checks and real-provider calls have not been run.
  Historical editor interception and desktop input restoration have compile
  checks/static inspection only, not GUI E2E evidence.
