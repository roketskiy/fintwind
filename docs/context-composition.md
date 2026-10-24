# Context composition

The context inspector shows provider-reported occupancy separately from the
estimated composition of the latest assembled primary request. The treemap
compares the estimates with each other. Its area does not represent the model's
capacity, and the estimates are never scaled to match provider usage. Available
capacity stays beside the measured occupancy above the chart.

| Category | Captured content |
| --- | --- |
| System and rules | Request system parts, system messages, skill and synthetic instructions |
| Tool definitions | Advertised tool names, descriptions and input schemas |
| Conversation | User and assistant text |
| Reasoning | Visible reasoning text |
| Tool calls | Tool names and input arguments |
| Files | Text attachments and results of `read` or `read_file` |
| Other tool results | Remaining tool results, including command output and structured errors |
| Compaction summary | Current replacement messages identified by completed compaction IDs, plus canonical compaction parts |

The OpenCode context hook supplies the assembled request. `session.context`
supplies message provenance; it does not contribute additional text to the
estimates. This keeps compaction summaries separate even when OpenCode lowers
them into ordinary user text. Files read indirectly through shell commands stay
in other tool results.

Text estimates use one token per four ASCII characters, rounded up per block,
plus one token per non-ASCII character. They exclude transport framing, media
and opaque provider state. Encrypted reasoning and native compaction state are
marked unmeasured; their encoded length is never treated as a token count.
Visible text accompanied by opaque state is shown as a partial estimate.

Version 2 snapshots split tool definitions, calls, results and reasoning. Older
persisted snapshots remain readable, but the detailed chart waits for a new
request instead of interpreting old combined totals as new categories.

## Verification

Run `python scripts/context-opencode-e2e.py`. It creates an isolated OpenCode
database and environment, random loopback ports, a local scripted provider and
an owned Windows Job Object. It checks category separation, official compaction
projection, opaque reasoning, session isolation and snapshot persistence after
restart. It never uses an installed Fintwind instance or a user's OpenCode
database. Artifacts are written under `target/context-opencode-e2e/`.
