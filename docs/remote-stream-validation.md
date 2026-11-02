# Remote stream validation

Use synthetic sessions for transport fixtures. For real-record rendering, use
only private, read-only captures on loopback origins and never commit the data.
Do not send prompts or approvals to a paired desktop. No process cleanup is
needed: the static preview owns no children and expires after 15 minutes.

Failure cases to cover before implementation:

- Intermediate assistant commentary incorrectly gets a timestamp/copy footer.
  While streaming no assistant part in the current turn owns a footer; after
  settling only its terminal answer does. Historical final answers remain usable.
- Activity groups must fold at the next assistant reply, not after each tool.
  A later activity starts a new expanded segment. Manual expansion/collapse
  must win over automatic boundary changes; no records may be reordered.

- A child-list mutation with unchanged row count selects an old user anchor.
- A temporary read failure unmounts the transcript and resets its position.
- Streaming, activity completion, highlighting, and new rows steal an up-scrolled
  reader's position; following at the bottom stops working.
- A shifted history window changes activity group identity or removes a visible
  row. Removed content cannot preserve an anchor; surviving rows must stay keyed.
- A delta arrives without its base, arrives for an old subscription/session, or
  uses an invalid path, oversized string, invalid revision, or forbidden key.
- A hidden page, disconnect, superseded subscription, or stopped remote control
  leaves a producer running or delivers stale data to the new conversation.
- A slow consumer grows an unbounded queue; unchanged snapshots consume network.
- An old desktop/client receives unsolicited new protocol frames.
- Hundreds of consecutive activities all mount by default, or a moving tail
  removes activities while the reader is inspecting them. Loading earlier rows
  must preserve the selected range as new activities arrive.

Repeatable scroll reproduction: build `mobile/vite.visual.config.ts`, run
`bun scripts/serve-mobile-visual.ts` with a 910-second timeout, and open the
printed loopback origin's `/visual-frame.html?scene=stream` in the in-app browser.
It uses the production Conversation and 120 history rows, then updates every
200ms. Observe the viewport's `scrollTop`, append and immediately remove an empty
child of the message content, and observe again after two animation frames.
Before the anchor fix this moved from 20998px to 0px. The fixture has no network
connection and cannot send messages.

## Hook and view checks

Build `bunx vite build --config vite.remote-fixture.config.ts` from `mobile`,
then serve with the same bounded static preview command. Open the printed origin
at `/remote-fixture.html`. This separate build aliases only the public client
boundary to `src/fixtures/remote-client.ts`. It runs the production `useRemote`,
`RemoteView`, Conversation and scroll primitive at a 390px layout width. It has
no real pairing, sockets, daemon calls, or network writes. Production Vite inputs
exclude all fixture pages and code.

The visible fixture controls support these repeatable checks:

1. Scroll up during updates. Compare a visible row's bounding-box offset across
   nine updates, and check that the viewport node is unchanged after `readError`.
2. `heldRead`, then `hide`, then `releaseRead`: a completed hidden read must not
   start a watch. While hidden, reads and deliveries must not advance. `show`
   must restore exactly one watch.
3. `mutationError` must make one failed mutation and then a fresh read, without
   replacing the viewport. `streamError` once must re-read and re-subscribe;
   twice within that effect must fall back without new watches.
4. Open `?legacy=1`: reads continue at the old interval, with zero watches.
5. `completeActivity` must not collapse the current turn's branch, including
   at the bottom: a completed tool does not finish its turn. `appendActivity`
   must not produce a collapse/reopen flicker. `waitForApproval` also keeps the
   branch open. Only `finishTurn` folds automatically at the bottom. While
   up-scrolled, finishing the turn must not collapse what the reader is
   inspecting or move the viewport. Return to latest: the settled branch folds.
   Manual expansion/collapse must survive activity and turn completion.
6. Open `?activities=1`: a 100-item branch must mount only items 96–100. Append
   an activity while following: only items 97–101 mount. Scroll to the branch and
   load earlier: items 92–101 mount. Append again while reading: the same ten
   items remain, with zero viewport offset change. `最新 5 项` restores 98–102.
   Both range controls must retain a 44px hit height. Group keys follow their
   preceding message, not the first activity that a bounded projection can evict.
7. Open `?footers=1`: `progress-before`, `progress-after`, and the streaming
   `live` message must each have zero footer/time/copy controls. Run `finishTurn`:
   only `live` gains one footer, one timestamp and one copy button. Intermediate
   messages still have none. Historical final answers retain their copy button;
   user timestamps are unaffected. The fixture supplies real-shaped metadata,
   including `streaming` and a measured zero-or-positive `elapsedSeconds`.

`document.documentElement.dataset.fixtureState` exposes counters and
`dataset.fixtureApp` exposes the displayed state. They work with the in-app
browser's isolated evaluation world; page-world globals do not. These attributes
are fixture-only. Save observed measurements in
`target/remote-stream-blackbox/browser-report.json`.

## Workspace capsule

Use the private real-record preview, with `workspaceChanges` obtained by the
shipping `git_branch::inspect` for that recorded session's resolved placement.
Never substitute the current desktop selection or a hand-written count. This
checks the recorded UI and real read-only Git inspection, not live subscription
transport. New desktop code still needs a rebuild before installed clients can
receive the field.

Check these boundaries before live validation:

- A remote session in another worktree must not display the selected desktop
  worktree's totals. A placement change or stopped activation rejects late work.
- Unknown, non-Git or failed inspection is null; a known clean tree is +0 / -0.
- With no todo list, totals remain readable without a dead expansion control.
- The one capsule keeps its 44px hit area, 8px corners and 96px collapsed-pill
  scroll clearance. Opening its todo panel must leave padding, viewport height
  and scroll position unchanged.
- Reads and active watches share a bounded background refresh per workspace;
  unchanged totals must not cause a desktop notify or a new encrypted payload.
- Hidden or closed subscriptions do not start further workspace inspections.

Keep private captures outside Git and save observed counts and layout metrics
under `target/` as local validation artifacts.

## Cross-language encrypted integration

First build `cargo build --locked --package fintwind-remote --example stream_fixture`
with a suitable build timeout. Then run `bun scripts/remote-stream-blackbox.ts`
from the repository root with a 30-second command timeout. It launches only the
prebuilt pure-computation fixture, with a ten-second bound, no descendants, an
explicit environment whitelist, and a unique artifact working directory. It
does not launch or terminate Cargo/rustc descendants. The producer uses the
shipping snapshot/delta and AES/HKDF implementations. The receiver uses shipping
phone crypto, parsing and reconciliation. It compares all 60 reconstructed
sessions with full projections, including Unicode, history shifts, appended and
removed activities, permission changes and completion. Six malformed/replay
cases must be rejected. No unit tests or running application services are used.

The script writes `report.json` and `browser-fixture.json` into
the unique `target/remote-stream-blackbox/run-<UUID>/` directory printed at completion.
One observed run transmitted 162332 plaintext
bytes instead of 7523542 bytes of repeated full projections (97.84% less), with
58 unchanged-history reuse checks passing. This is synthetic codec integration,
not a measurement of real phone power, desktop UI CPU, relay latency, or the
native subscription producer. Those still need live desktop/relay validation.
