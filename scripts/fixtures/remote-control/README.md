# scripts/fixtures/remote-control — synthetic provider for the acceptance chain

A synthetic OpenCode provider used only by the remote-control acceptance
chain. It speaks the slice of the OpenCode HTTP + SSE contract the Fintwind
driver exercises for one scripted turn, including the two interactive
surfaces the remote chain must accept through the real wire:

| Surface | Provider event | Reply route | Reply body |
| --- | --- | --- | --- |
| Tool approval | `permission.requested` | `POST /api/session/{sid}/permission/{id}/reply` | `{"decision": "once"\|"reject"\|"always"}` |
| Question set | `form.created` with `metadata.kind == "question"` | `POST /api/session/{sid}/form/{id}/reply` | `{"answer": {field: value \| [values]}}` |

Everything runs on loopback TCP. There is no real provider, no model key,
and no network access. `scripts/fixtures/opencode-recovery/fake_opencode.py`
is the reference this was derived from; it is never modified.

## Files

- `synthetic_opencode.py` — the provider. Requires `python3` on PATH.
- `launch_isolated.py` — re-execs the provider under a minimal whitelist
  environment. The shim always goes through it: the provider never inherits
  a user profile, a global OpenCode config, or real credentials, and the
  spawn log records the variable **names** it kept as the evidence.
- `acceptance.json` — one turn that asks for an approval, then a question
  set with a single-select and a multi-select field, then finishes.
- The provider is started through an `opencode.cmd` shim exactly the way
  the recovery fixture does it, so the driver spawns it the same way it
  spawns a real npm-installed `opencode`.

## Timeline contract

The scripted turn follows the event order a real server publishes:

1. `session.execution.started` — the driver marks the turn active on it
   (without it no turn can settle);
2. `session.text.started` / `session.text.delta` / `session.text.ended`;
3. `permission.requested` — the driver asks, and the timeline blocks on
   `await_reply` until the decision lands on the reply route;
4. `form.created` with `metadata.kind == "question"` — same blocking
   contract;
5. `session.execution.succeeded` followed by `session.idle`.

Durable history (`GET /api/session/{id}/message?limit=5`) restates the
acknowledged native message id and the real prompt text on the user row,
newest-first, the way a real server persists client messages.

## Environment

| Variable | Meaning |
| --- | --- |
| `FINTWIND_RC_BEHAVIOR` | path to the behavior manifest |
| `FINTWIND_RC_LOG` | native request log (JSONL) |
| `FINTWIND_RC_SPAWN_LOG` | spawn record (JSONL): proves `serve`-only argv |
| `OPENCODE_SERVER_PASSWORD` | the private Basic auth password the driver injects |

## Logging contract

The native request log records the **path alias, a counter and the request
id only**. Prompts, option labels, permission patterns and answer bodies
are never written. A run artifact may therefore be shared without
redaction.

## Timeline control

`await_reply` steps block the scripted timeline until the named reply
lands, so an approval or an answer is observed by the driver before the
turn continues. A reply that never arrives times the step out and ends the
stream, which the harness records as a failure rather than hanging.

## Running it

`fintwind-pwa/scripts/provider-chain-blackbox.mjs` is the runner. Since the
process-ownership incident it never kills anything by name, port, command
line, directory or PID, and never scans for or adopts orphans. It is
blocked by an incident gate (a `throw` near the top) that only main
removes, after reviewing and independently running the containment design
below.

* it spawns exactly one process itself — the Rust `acceptance_supervisor`
  (`crates/fintwind-remote-host/examples/acceptance_supervisor.rs`);
* the supervisor creates one Windows job object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; every child is created
  `CREATE_SUSPENDED`, assigned to the job **by its creation handle**
  before its first instruction, and only then resumed — the daemon, the
  seed, the driver example, and the provider's cmd/python descendants all
  inherit the job;
* every end path (normal end, total deadline, spawn failure, supervisor
  crash) ends in the same kernel teardown of the whole tree; the deadline
  path additionally calls `TerminateJobObject` on the handle it owns;
* `containmentVerified` in the status file is only true when the job's
  own accounting reports zero active processes **and** every held child
  handle has been waited within a strict cleanup deadline — the runner
  fails the run without it;
* raw stdout/stderr never reaches disk: readiness is parsed in memory from
  the exact pipe the supervisor holds (bounded, verified against the
  child's own pid/protocol/loopback), stderr is counted in memory and
  discarded. Status contains step labels, exit codes and verification facts,
  not readiness tokens or raw output. The seed writes a separate structured
  fixture file containing only synthetic IDs and the fixture marker;
* the runner polls the status file; it never copies child text into the
  artifact.

The incident gate is the `throw` at the top of the runner; only main
removes it, after reviewing the containment design and running the
synthetic process-ownership black box (below) independently.

## Synthetic process-ownership black box (for main, after review)

A containment check that runs no provider, no daemon and no browser:

1. Build `acceptance_supervisor` (`cargo build --locked -p
   fintwind-remote-host --example acceptance_supervisor`).
2. Write a plan whose steps are three trivial Windows children — e.g.
   `cmd /c timeout` variants, one `spawn` with `readyFromLog` printing a
   ready-looking JSON line, one `run` that exits 0, one `run` that exits
   3 — with a short total deadline.
3. Assert from the status JSONL: every step is recorded, the failing step
   names itself, `containmentVerified` is true only when the job
   accounting reached zero and every handle was waited, and the final
   event always exists — including on the deadline path.
4. While the long-lived child runs, verify from outside (a separate
   observer process of the reviewer's choosing) that the whole tree sits
   in one job and that killing the supervisor ends every descendant.
5. Only after these pass does the functional gate lift for the real
   `--local` slice (daemon + seed + driver + synthetic provider).

## Residual risks

- The supervisor is Windows-only by design; a non-Windows run refuses to
  start rather than degrade.
- If the runner is killed before the supervisor finishes, the supervisor
  still enforces its own total deadline and drops the job; the kernel
  closes a dead process's job handle with the same effect.
- The paired/phone slice is not wired through the supervisor yet: its
  mid-flight pairing confirmation needs a containment design of its own
  and stays behind the gate.
- The daemon's provider resolution is its process `PATH`; the harness
  isolates that PATH per run (fixture directory first). Nothing global is
  touched, and no production daemon or OpenCode API is ever a target.
