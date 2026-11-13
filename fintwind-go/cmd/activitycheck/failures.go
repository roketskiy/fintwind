package main

// Failure-first E2E acceptance cases. These assertions are implemented in
// main.go before any pass is reported; none is a unit test or source-only pass.
//
// 1. An unknown tool.progress, started without id, or started without name
//    must not emit richActivity, start a provider turn, or fabricate activity.
// 2. Tool started/called/progress/success events use one source_id and one
//    activity id; called/progress may omit name and must retain the first name.
// 3. A terminal event removes the active tool. A late progress cannot change
//    complete/failed back to running or mutate the terminal output.
// 4. tool names read/bash/edit/grep/todo and mcp__provider__read classify as
//    fileRead/command/fileChange/fileSearch/plan/fileRead. create_thread and
//    read_mcp_resource remain generic tool, never fileRead.
// 5. Error payload {'message': ...} is rendered as the message, without JSON
//    object braces/private provider control markers.
// 6. Text and tool richActivity events retain provider order; called input is
//    preserved once, not duplicated from a repeated event name.
// 7. SQLite hydrate after settlement and process restart must retain ordered
//    activities. Native recovery uses OpenCode message content parts shaped as
//    {type:'tool', callID, tool, state:{status,input,output,error,title}}.
//    Recovered type, args, output, failure state must match live values.
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
