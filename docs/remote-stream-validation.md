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
  removes activities while the reader is inspecting them. Explicitly expanding
  all activities must retain that choice as new activities arrive.
- Long reasoning opens at the top, grows beyond its height cap, or stops following
  appended output at the bottom. Scrolling upward must pause follow; returning
  to the bottom must restore it. Inner scrolling must not move the transcript.
- On refresh, the capsule appears before the pending transcript scroll completes,
  clearance arrives late, or separately loaded composer code resizes the viewport.

Repeatable scroll reproduction: build `mobile/vite.visual.config.ts`, run
`bun scripts/serve-mobile-visual.ts` with a 910-second timeout, and open the
printed loopback origin's `/visual-frame.html?scene=stream` in the in-app browser.
It uses the production Conversation and 120 history rows, then updates every
200ms. Observe the viewport's `scrollTop`, append and immediately remove an empty
child of the message content, and observe again after two animation frames.
Before the anchor fix this moved from 20998px to 0px. The fixture has no network
connection and cannot send messages.

## Hook and view checks

Mobile UI correction failure cases (before implementation, 2026-11-03):

- Provider groups wrap into a wall of chips, squeeze model names on a 320px
  screen, or make the popup height jump. Both columns must scroll independently;
  long provider/model names must not overflow, and keyboard focus stays visible.
- Search loses cross-provider matches, favorites lose desktop ordering, selecting
  a model closes on failure, or a rapid double click submits twice.
- Several reminders cover the transcript with large cards. The default surface
  must be one compact row, with explicit access to queued reminders and session
  navigation. Closing a reminder must not activate the session behind it.
- TeX bracket delimiters are unescaped by Markdown before reaching KaTeX;
  one-line display math becomes inline math, or common equation environments
  stay raw. Existing dollar math must still work, including aligned equations.
- KaTeX markup and stylesheet resolve different versions, equation tags collide,
  inline formulas dominate the body text, or wide display math shrinks to an
  unreadable size. Verify font sizes, tag geometry and horizontal scrolling.
- A delimiter normalization rewrites fenced, indented or inline code, escaped
  backslashes, link targets, or TeX already inside dollar math. Streaming partial
  expressions must not swallow subsequent prose. Unsafe math remains untrusted.
- A diagram smaller than the viewport stays left-aligned. Center fitting images
  with equal side margins; oversized images must retain their width and expose
  both edges through scrolling, without negative margins hiding the start.

Use the isolated workflow fixture and save a correction-specific artifact. Check
actual screenshots in both themes and a narrow viewport; do not deploy until the
user has seen and approved the revised UI.

Mobile workflow completion failure cases (before implementation):

- Page-open reminders announce old completed history, repeat on every refresh,
  confuse a stopped task with success, or open a same-ID task in the wrong
  workspace. Hidden pages must not poll; disconnected/legacy desktops must not
  trigger repeated unsupported requests. Monitor only bounded in-memory metadata.
- A task failure or operation error flashes away on the next stream update,
  leaks credentials, or shows a notice from an older turn as the current failure.
- Model groups differ from desktop provider labels/favorite ordering, search
  excludes other providers, or a failed selection closes the picker.
- A question's Check button becomes Submit during the same click and submits
  without a separate confirmation; implicit form submission, a double tap, or
  identical streamed questions resets answers or briefly shows/closes review.
- Math renders in code, unsafe LaTeX commands create links, malformed math
  crashes the transcript, or a wide equation overflows the screen.
- Mermaid markup executes script/links/directives, malformed/streaming diagrams
  crash the transcript, queued renders outlive their source, or expensive work
  is repeated on unchanged stream frames. Render generated SVG as an inert image;
  keep source/copy fallback and bounded lazy rendering. No external assets.
- Composer clearance changes keyboard/safe-area behavior or overlays controls.

Use isolated workflow fixture controls for reminder transitions, cross-workspace
navigation, model groups, persistent failures, question review/submit and rich
Markdown. Do not send real prompts or approvals. Keep repeatable blackbox results
in `target/`; builds alone do not prove interaction or live provider behavior.

2026-11-03 isolated workflow results are in
`target/mobile-workflow-validation.json`, including repeat instructions. Open
`remote-fixture.html?workflow=1` after a remote-fixture build. The fixture exposes
completion/failure/permission/question actions, injected next-mutation failure,
visibility controls, a cross-workspace target, ordered favorites and rich Markdown.
Question selection/review sent nothing; a failed submit retained the answer, and
an explicit double click sent one successful answer. Workspace navigation,
reminder deduplication/visibility pause, grouped model search and failed model
selection were exercised without real pairing or provider requests.
Opening a reminder for the already-open session keeps its generation unchanged;
the final check observed a later status transition through the same subscription
without a replacement watch or read.

The workflow preview uses production script/style/font restrictions, with
`connect-src 'none'` to prohibit real traffic. KaTeX fonts stay same-origin files;
math and Mermaid dependencies are absent from a no-math fixture's resource list.
Unsafe LaTeX links/images and Mermaid click/URL sources stayed inert. The earlier
Mermaid.js implementation hit CSP restrictions on temporary measurement styles.
It has since been replaced with lazy Pele rendering and has no measurement DOM.
Correction checks covered flowcharts, sequence diagrams, cached-layout theme
changes and unsafe-source fallback without CSP violations. Keep the production
CSP unchanged. The correction evidence is in
`target/mobile-corrections-validation.json`; the centering follow-up is in
`target/mobile-mermaid-centering-validation.json`.
Clipboard read-back timed out and is not counted as a passed check. Real mobile
keyboard/safe-area behavior and the new desktop projection remain separate live
verification work; this run did not rebuild or restart the installed desktop.

Session-usage capsule failure cases (before implementation):

- Cumulative throughput is shown as context occupancy, or cached tokens are
  divided by total output-inclusive throughput instead of cumulative input.
- A configured context-window override differs from the desktop meter. Missing
  counters, an unmeasured zero, or zero prompt tokens appear as measured 0%.
- Full snapshots or deltas discard counters; negative, fractional or unsafe
  numeric values pass the phone trust boundary.
- The right-hand usage trigger toggles todos, both panels overlap, Escape loses
  focus, or opening either panel changes the transcript viewport/scroll offset.
- No-todo, unknown-data, over-capacity and narrow-screen states hide the meter
  or overflow the viewport. A stream update resets an open usage panel.

Use `/remote-fixture.html?usage=1` for production-view interactions; repeat with
`usage=unknown`, `usage=zero`, `usage=over`, `usage=only` and `usage=legacy`.
Open the usage trigger, observe updates, switch to todos, close with Escape and
check focus/scroll geometry. The known case is 42,000/200,000 context tokens,
1,234,567 cumulative tokens and 80% cumulative cache hits (800,000/1,000,000).
The encrypted codec blackbox also carries usage through snapshots/deltas and
rejects malformed counters. These are isolated boundaries, not live provider
evidence; do not send real prompts or approvals for this check.

Completed-turn disclosure failure cases (before implementation):

- Only the last activity block collapses instead of the assistant's earlier
  progress and work sharing one disclosure. The opening user prompt must always
  remain outside the disclosure; same-turn steering messages may be folded.
- A non-streaming intermediate reply, a completed tool, approval waiting, or a
  same-turn steering prompt is mistaken for the final response.
- The final response (including its copy/time footer) is hidden with the process,
  or a truncated turn is merged with a different turn.
- Manual reopening is reset by subscription updates or the next turn. A reader
  above the latest output loses the inspected content when the turn settles.
- Legacy records without turn metadata stop rendering or fold a running turn.

Use `/remote-fixture.html?folding=1` for isolated production-view checks. Finish
with `finishTurn`: progress, steering and activity rows must share one closed
disclosure while the opening user prompt and final reply remain visible. Reopen
using its real trigger (including keyboard), observe later deliveries, then use
`nextTurn` to ensure the next running turn is not folded. Repeat with `legacy=1`,
`settled=1`, `truncated=1`, and `noFinal=1`. Above-latest readers defer automatic
folding until they return to the bottom. These are rendering-boundary checks,
not provider/relay end-to-end evidence. Keep observations in `target/` only.

Permission-setting failure cases (before implementation):

- Old desktop capabilities display an unusable permission control or imply a
  default permission that was not received from the desktop.
- A permission change affects the selected desktop session instead of the
  requested remote session, changes Build/Plan, or is not persisted/applied.
- Invalid, legacy, null, or unknown runtime modes bypass request validation.
- Busy/loading/unconfirmed sessions accept a setting change, or failed changes
  optimistically display a new permission that the desktop did not accept.
- Full snapshots or deltas lose the current permission; reopening shows a stale
  selection. Legacy requests without runtimeMode must continue to deserialize.

For the isolated client boundary, use `?permissions=1` to expose the production
options menu and record configuration fields without real pairing or writes.
Verify all three choices and reopen after each accepted change. Use
`?permissions=1&legacy=1` for no permission control and
`?permissions=1&busy=1` for disabled controls. These checks are not live desktop
permission enforcement. Do not send real approvals or prompts to validate them.

Live-output latency failure cases (before implementation):

- Every notification pays a full batching delay, including the first update
  after an idle period; socket reads add another full idle polling interval.
- Faster output raises desktop projection/render cadence above the 120ms stream
  budget, sends unchanged payloads, or creates an unbounded pending queue.
- Legacy active sessions remain on a 2.5-second refresh, but speeding them up
  also accelerates idle/error/background reads or overlaps slow requests.
- Ending/hiding a subscription retains its fast socket-read setting or starts
  fallback polling while a subscription is already delivering updates.

Before changing the hook, observe `fixtureState.reads` in `/remote-fixture.html?legacy=1`
with a MutationObserver and retain timestamps across at least three reads. Repeat
afterwards for running, settled (`finishTurn`), hidden, held-read and failed-read
states. A subscribed page must not poll. This is a hook scheduling measurement,
not live phone/relay latency. Keep baseline and changed observations in `target/`.

Remote steering failure cases (before implementation):

- A busy phone send silently queues a follow-up or steers the desktop's selected
  session instead of the requested session. Only the requested live runtime may
  receive it; unsupported, connecting or settled sessions must reject it.
- The phone clears the draft or reports delivery before SteerAccepted. A rejected
  remote steer must return an error and retain the draft, not fall back to queueing
  or start a new turn. Existing desktop steer fallback must remain unchanged.
- Attachments, duplicate prompt text, a stopped runtime, or lost acknowledgements
  assign a reply to the wrong pending steer or cause automatic resubmission.
- Old desktops receive the new command despite lacking its capability. Hidden
  pages continue polling; timed-out requests are blindly retried.

The 500ms update target includes the network and is a target, not a hard guarantee.
Measure hook scheduling separately from actual phone/relay delivery. A 300ms
legacy active-session pause leaves roughly 200ms for network and rendering only
under suitable network conditions. Desktop subscription projections stay at most
once per 120ms, with no fixed delay after idle; active socket reads use 50ms.

For isolated steering UI behavior open `/remote-fixture.html?steering=1`, enter a
draft and use the real `发送引导` button. Until `acceptSteer`, the draft must remain
and the composer reports pending delivery; acceptance appends one user message
and clears the draft. Use `rejectSteer` on a second draft: the draft and a visible
error must survive subsequent subscription updates. Neither case creates a queue.
`?steering=1&unsupported=1` and `?steering=1&legacy=1` must disable busy sending.
This boundary simulates provider acknowledgement only; it does not validate the
Rust dispatcher or real provider execution. For latency use
`?legacy=1&latency=active&readMs=140`; fixtureState exposes bounded readTimes and
maxInFlightReads. Retain the measured intervals and scope in target artifacts.

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
    choose `全部展开`: items 1–101 mount in one action. Append again while reading:
    items 1–102 remain available without resetting the all-items choice. `最新`
    restores items 98–102. Without expanding all, up-scrolled readers retain their
    selected five-row window until returning to latest.
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

## Reasoning and refresh layout

Use the private recorded preview for real grouping, branch geometry, and capsule
layout. Capture frame changes for the first 120 animation frames: outer viewport
height, scrollTop, pending-scroll, visibility, bottom padding, capsule visibility,
and composer height. The capsule must remain hidden while the outer viewport is
pending; the first visible transcript must already have its 96px clearance and
final composer height. Opening the capsule must not alter these measurements.

If captured reasoning is too short to overflow, run a separately labelled local
component check: replace only the mounted reasoning item's DOM text with numbered
lines, append lines at the bottom, scroll up with a wheel/keyboard event, append
again, then return to the bottom and append once more. Record the inner viewport
height, scrollHeight, scrollTop, and outer scrollTop at each stage. This checks the
production scroll primitive and CSS only, not React streaming or live transport.
Reload the private preview afterwards so the displayed conversation is unmodified.

Check lazy loading separately with `/remote-fixture.html`, which runs RemoteView
but uses a synthetic transport boundary. Do not present its content as real
conversation evidence. Production phone/desktop/relay validation is still separate.

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
removed activities, approval changes, all three runtime permission modes and
completion. Seven malformed/replay
cases must be rejected. No unit tests or running application services are used.

The script writes `report.json` and `browser-fixture.json` into
the unique `target/remote-stream-blackbox/run-<UUID>/` directory printed at completion.
One observed run transmitted 162332 plaintext
bytes instead of 7523542 bytes of repeated full projections (97.84% less), with
58 unchanged-history reuse checks passing. This is synthetic codec integration,
not a measurement of real phone power, desktop UI CPU, relay latency, or the
native subscription producer. Those still need live desktop/relay validation.
