# Mermaid visualization preference

The global default is off. Changing it applies only to newly created Fintwind
sessions, not to existing sessions (even an empty session created earlier).

New sessions pick the latest prompt version (`v2`). Previously issued versions
stay byte-identical forever: a `v1` session keeps the `v1` instruction, across
forks, restarts and releases. `v2` differs from `v1` only in the relaxed
syntax preference and one added line that tells the model to validate every
diagram it writes with the `fintwind_mermaid_check` tool.

## Mermaid check tool

Sessions carrying a Mermaid prompt version (any version) also receive the
`fintwind_mermaid_check` tool. It is registered by the same context plugin
when the daemon exposed the check capability to the private OpenCode process;
sessions without a Mermaid prompt version never carry it in their tool
snapshot. The tool validates one source against the identical compiler the
app renders with (`crates/fintwind-diagram`), so "the tool says valid" and
"the fence draws" are the same fact. See `docs/diagrams.md` for the wire.

## Failure cases, before implementation

- Reading the live setting on each model request changes an existing prefix.
- Copying the setting at first submission instead of session creation affects
  an already-created empty session after a toggle.
- A missing field in old saved sessions accidentally enables the preference.
- Reopening a session, restarting its driver, or compacting loses its choice.
- Changing the prompt text for an existing version invalidates the prefix.
- Forking applies the new global default instead of the parent's frozen choice.
- A metadata lookup error silently drops instructions for one request.
- A hook duplicates the instruction on tool continuations or mutates history,
  tools, title generation or compaction prompts.
- Saving the global switch restarts a running driver or reloads tools.

## Contract

New sessions snapshot the latest version (`v2`) or no preference. The OpenCode
session receives that version as creation metadata. The always-loaded context
plugin appends the corresponding constant instruction to primary model requests
only; it never reads the global default. No version means no changes to the
outgoing request. Existing prompt versions must stay byte-identical in future
releases, so `v1` sessions keep the `v1` bytes even though new sessions pick
`v2`.

Provider cache hits also depend on the model/provider and other context changes;
this feature promises not to change existing requests because of its switch,
not an unconditional vendor cache-hit guarantee.

## Verification

```sh
cargo check --locked --offline --workspace
cargo run --locked --offline --package fintwind-core --example mermaid_prompt_blackbox -- target/mermaid-prompt-blackbox
cargo run --locked --offline --package fintwind-core --example mermaid_check_e2e -- C:/Users/Public/Temp/opencode/mermaid-check-report.json
python scripts/mermaid-prompt-e2e.py --opencode E:/bun/bin/opencode.exe --deadline 180
```

The Rust scenario writes a report and isolated SQLite database under a new
`target/mermaid-prompt-blackbox/<uuid>/` directory. Its 13 checks cover default
off, creation-time selection, changes during a running turn, future defaults,
legacy data, fork inheritance, list projection, SQLite reload and hydration.
It does not launch the UI or exercise keyboard interaction.

The provider scenario uses a private OpenCode database, a whitelisted
environment, a loopback scripted model and an owned Windows Job Object. It
never connects to the installed app's daemon or a real model. Its report
records request-prefix hashes, not a claim of vendor-side cache hits.

Verified with OpenCode `2.0.32`: 22 provider checks passed (multi-turn off/v1,
unchanged old-session system bytes, unchanged tool schema/history prefix,
exactly one instruction on tool continuations, fork and process restart).
Direct metadata-lookup failure injection and forced compaction remain untested;
unknown versions were verified to dispatch no primary model request.

Artifacts for this run:

- `target/mermaid-prompt-blackbox/c2675364-a922-43d7-b245-d1e5a4c43ce1/report.json`
- `target/mermaid-prompt-e2e/c6da952a-8388-4d2e-8cbf-d3e76b678907/report.json`

The preference-file fixture only verifies that the plugin ignores a changing
external default. Actual client-side preference snapshotting is covered by the
Rust scenario, not by that fixture. No native UI acceptance or real-provider
cache measurements have been performed.

Keep the fixed `v1` prompt unchanged after release. To revise the instructions,
introduce a new version and retain all previously issued versions.
