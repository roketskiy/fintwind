package main

// Failure-first E2E acceptance cases. These assertions run against the owned
// daemon/fake CLI/SQLite flow; none is a unit test or source-only pass.
//
// 1. An unknown tool.progress, started without id, or started without name
//    must not emit richActivity, start a provider turn, or fabricate activity.
// 2. Execute started/called/progress/success events use one source_id and one
//    activity id; input.code remains arguments and ordered toolCalls are only
//    projected to the parent display target (capped at 240 runes).
// 3. A terminal event removes the active tool. A late progress cannot change
//    complete/failed back to running or mutate the terminal output.
// 4. Tool names read/execute/edit/grep/todo and mcp__provider__read classify as
//    fileRead/command/fileChange/fileSearch/plan/fileRead. apply_patch is a
//    fileChange; read_mcp_resource remains generic tool, never fileRead.
// 5. Error payload {'message': ...} is rendered as the message, without JSON
//    object braces/private provider control markers.
// 6. Text and tool richActivity events retain provider order; called input is
//    preserved once, not duplicated from a repeated event name.
// 7. SQLite hydrate after settlement and process restart must retain metadata.
//    Native recovery uses OpenCode message content parts shaped as
//    {type:'tool', callID, tool, state:{status,input,output,error,title}} plus
//    public metadata fields. Recovered type, args, output, display metadata,
//    image URLs, file changes, and failure state must match live values.
// 8. Invalid tool events form no synthetic turn/activity; terminal settlement
//    remains intact.
// 9. Windows terminal compatibility (no ConPTY/shell): openTerminal returns
//    exact unsupported-platform error; write/resize return exact not-running
//    error for the provided session; closeTerminal returns ack idempotently.
// 10. All requests traverse real authenticated daemon WS -> Owned fake CLI
//     loopback HTTP/SSE -> SQLite. No provider keys, model requests, OAuth,
//     user processes, or non-loopback traffic are allowed.
//
// Not claimed: nested CodeMode, subagent lifecycle, or child/background-work
// metadata. The separate authorized DeepSeek-low check is owned by main model.
