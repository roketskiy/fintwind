package main

// Failure-first acceptance matrix. The fixture is controlled source-built
// OpenCode-compatible HTTP/SSE data, not a capture from a real provider.
//
// Core lifecycle:
// 1. Root task started/called share one activity identity and the called event
//    does not overwrite the first tool name when its name is omitted.
// 2. Only session.created with parentID matching the native root may create a
//    child. Unrelated sessions and unknown child frames never enter root output.
// 3. Child text, reasoning and tool frames retain order and child scope. Child
//    terminal events settle only that child; root remains running.
// 4. Late child deltas and duplicate execution.started cannot revive a settled
//    child or create an extra turn. A root-bound task continuation must match
//    an exact known child target; task_id alone is not a target.
// 5. Continuation originActivityIds append uniquely; duplicate call/replay does
//    not re-arm. Pending target bindings are exact-first and root settlement
//    clears unfulfilled revival bindings.
//
// Controls and requests:
// 6. stop validates registry key/controlId; unknown/root/mismatched identities
//    fail without HTTP side effects. A valid child stop calls native interrupt,
//    never a shell. compactChildSession targets only that child native ID.
// 7. Child form/permission events are answered/cancelled on that child session.
//    Public daemon events expose only approved fields, not provider internals.
//
// Refresh and persistence:
// 8. Refresh is actor-owned/asynchronous; paginated roster, 400 fallback,
//    cursor cycle, active data shape and failures are checked. Invalid or
//    incomplete results never publish an authoritative empty reconciliation.
// 9. Valid refresh emits tagged reconcileLive and snapshots bounded transcripts;
//    slow HTTP does not block live SSE processing. Stale generation/revision
//    results are ignored.
// 10. SQLite save/hydrate/restart retains camelCase background_work and raw
//     transcriptBlocks content. A legacy ordinary save without the background
//     field cannot erase daemon-owned background state.
// 11. Fork clears background controls; rewind removes origins for discarded
//     turns; history edits while child work is live are refused.
// 12. Registry/pending/transcript/global capacity paths have explicit errors
//     or outputTruncated/detail semantics and never silently grow without bound.
// 13. An unknown historical target that emits execution/text/request events
//     before session.created is queued. A complete roster proving parentage
//     must activate it after either an empty transcript or an old transcript;
//     the queued first execution starts a fresh child turn and its deltas must
//     not overwrite old assistant text. Early permission/form events keep child
//     scope after roster reconciliation.
// 14. session.deleted settles a child as stopped and publishes exactly one
//     finished record. It clears outstanding request scopes: a late response
//     is expired and must not fall back to a root-session reply.
// 15. Refresh and SSE race: slow roster I/O must not block root SSE handling;
//     a stale roster result cannot mark a newly discovered child lost. An
//     incomplete/cyclic roster is not authoritative.
// 16. Capacity boundary smoke checks cover a >64 KiB prompt and a >64 KiB child
//     event. They must warn/truncate while the root turn remains usable.
// 17. A real HTTP roster with 76 metadata-heavy children (including Unicode
//     and JSON-escaped control characters) exceeds the complete 8 MiB snapshot
//     budget. Snapshot metadata/payload/escaping must remain within the loader
//     limit, mark truncation explicitly, preserve identity and ordinary root
//     content, then close/restart/hydrate without losing the roster. Short
//     role/model/prompt values and outcome/status mappings remain exact.
//
// Safety: real daemon WS -> Owned loopback fake CLI HTTP/SSE -> SQLite, protocol
// 25, session provider opencode and native cursor provider openCode. No real
// models, OAuth, credentials, external network, visual UI, or user processes.
// Nested CodeMode tool metadata is explicitly not covered by this batch.
