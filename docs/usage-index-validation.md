# Usage index validation

Failure cases recorded before implementation:

- Same-day A -> B -> A usage is reassigned to the selected model.
- Missing model metadata inherits the selected model.
- An amended message is counted twice.
- Restart downloads unchanged historical messages again.
- Concurrent readers start competing scans.
- A partial/failed listing deletes cached sessions.
- A failed message read erases verified history or becomes cached zero usage.
- Independent sources share session IDs and overwrite each other.
- Price-network latency blocks token snapshots.
- Date navigation traverses remote history.
- Sub-agents are counted twice or inherit the parent's model.
- A single unreadable or not-yet-synchronized session produces a persistent
  page-level error instead of being skipped while other usage stays available.

Verification uses an isolated module-level black-box provider and SQLite database.
It must not start or terminate installed Fintwind, daemon, or OpenCode processes.
The integration run writes a JSON artifact with request counts and snapshot times.
No unit tests, dev watcher, or visual tests are used.

## Implementation

- `usage-index.db` lives beside the daemon's task database. Versioned SQLite
  tables separate sources by binary/storage environment, not project or port.
- Complete session message facts (message ID, model, time and token mix) are
  stored as an atomically replaced session bundle. Aggregate snapshots are
  separate; a restart can reuse completed bundles even if a scan was interrupted.
- Fingerprints include the timestamp, all token categories, cost and selected
  model. Recent sessions also receive a five-minute reconciliation pass.
- Four workers process changed sessions, newest activity first. Progress is
  checkpointed in batches and published at most once per second.
- Malformed/incomplete/failed listings cannot delete verified history. A failed
  message walk preserves the previous split and remains eligible for retry.
- Individual session-history failures (including a message store that has not
  caught up with its summary) are logged and skipped silently. They do not set
  the snapshot's page-level error. Source/listing failures still report an error.
- Indexed usage with no model and summary/message discrepancies stay unknown;
  the selected model is not evidence of historical usage.
- Local/stale prices are immediately usable. Catalog downloads are independent;
  request-sized token facts preserve context-pricing tiers.
- Pricing and date-range aggregation run off the UI thread. Selecting dates
  changes a local view only; explicit refresh bypasses the daemon's freshness
  interval without starting a competing scan.

## Repeatable run

```sh
cargo check --locked -p fintwind-core -p fintwind --tests
cargo run --locked -p fintwind-core --example usage-index-blackbox -- C:/Users/Public/Temp/opencode/usage-index-validation
```

The executable allocates a fresh UUID-named database for each run and writes
`result.json` into the supplied artifact directory. It checks A/B/A attribution,
single-flight requests, restart reuse, replacement after amendment, failure
retention, cyclic pagination, sub-agent attribution, unknown models, independent
sources, deletion and a 1,000-session restart. Reported times measure the module's
snapshot read, not desktop first-paint latency or a P95 service guarantee.

## Remaining runtime validation

The harness does not validate the installed app, provider networking, visual
layout, or end-to-end desktop first paint. A first-ever index still needs to read
historical messages once; only verified summaries are available before that
backfill finishes. Unchanged history is reused on later starts.

## Recorded result (2026-10-16)

- `cargo check --locked -p fintwind-core -p fintwind --tests`: passed (compile
  check only; no unit tests executed).
- Black-box scenario: all 12 reported cases passed.
- Small snapshot read: 2.73 ms; 1,000-session restart snapshot read: 9.15 ms.
- Synthetic 1,000-session initial index: 205.09 ms including harness polling;
  unchanged restart performed zero additional message-history requests.
- Artifact: `C:/Users/Public/Temp/opencode/usage-index-validation/result.json`.

These are single-run local synthetic measurements, not desktop first paint or
network performance claims.

Follow-up: the same harness passed 14 cases after adding unsynchronized and
unreadable individual sessions. Both were skipped with background logs and no
page-level error, while the readable session retained its model attribution.
