# Long-session memory repair: failure matrix

Black-box checks must cover these failures before the repair is accepted:

- A slow save consumer must not retain one full transcript per save attempt.
  Only one desktop save may be in flight; later edits stay dirty in the live
  state and the next snapshot must include all of them.
- A failed save must restore the submitted dirty ids without clearing newer
  edits. A completion wakes the event pump even after streaming ends.
- The quit-only `flush` barrier must wait for a pending save and persist the
  final snapshot before shutting down the desktop-owned daemon. The GUI hook
  remains best-effort within GPUI's shutdown deadline, as noted below.
- Settings and window-state writes must run off the UI thread and unchanged
  files must not be rewritten on every stream checkpoint.
- Save confirmations must not retain full child-agent history in the daemon's
  replay-response cache. Explicit RPC callers still receive their full result.
- Unchanged session detail must not execute an UPDATE. Message changes must
  still persist independently. Fingerprints are installed only after commit;
  skeletons must never replace hydrated history and deletion drops fingerprints.
- A child transcript must build only viewport rows, keep message/block order,
  remeasure changing rows and disclosures, and retain keyboard scrolling.
- Markdown and reasoning render caches must be bounded while staying in one
  session. Closing a card releases its parsed/rendered detail.

Run all persistence checks against unique directories under
`C:/Users/Public/Temp/opencode`. Use a loopback listener with port zero and an
environment whitelist. The harness must never discover or terminate user
processes. Record counters and final persisted content in a JSON artifact.
No visual tests or development watcher are part of this check.

## Repeatable checks

Run in a clean checkout containing this repair:

```powershell
cargo check --locked --package fintwind --package fintwind-daemon
cargo build --locked --package fintwind-client --example memory_save_blackbox
pwsh -File scripts/check-memory-save.ps1
cargo build --locked --manifest-path scripts/fixtures/memory-storage/Cargo.toml --target-dir target/memory-storage
pwsh -File scripts/check-memory-save.ps1 -Binary target/memory-storage/debug/memory-storage-blackbox.exe -Label memory-storage
```

Each run prints its unique artifact directory. `result.json` records behavior
checks, `memory.json` contains process-private-memory samples, and `stdout.txt`
and `stderr.txt` preserve diagnostics.

These checks verify persistence and backpressure. They do not
prove hours-long GUI memory usage or visual/keyboard behavior. The desktop
still copies parent-session data once per admitted checkpoint; completed child
snapshots share immutable history. Ordinary RPC queues are not bounded by this
repair. Detail fingerprints skip unchanged writes, not JSON serialization.

The pinned GPUI revision (`f9bad894`) gives `on_app_quit` futures only 200 ms
(`gpui/src/app.rs`, `SHUTDOWN_TIMEOUT`). `StateStore::flush` orders the commits,
but a slow daemon can exceed that GUI deadline. The isolated barrier check
does not establish lossless GUI exit under an arbitrarily slow daemon.
