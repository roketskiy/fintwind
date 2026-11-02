# Remote control protocol

This reference defines Fintwind remote protocol 1. It is not Alma's encryption protocol. Alma Relay only carries the outer tunnel frames. The desktop accepts `/__fintwind_remote__` WebSocket opens and rejects every HTTP request and other WebSocket path.

## Pairing and encryption

Every activation generates two independent 32-byte random values. The routing token identifies the tunnel. The pairing secret authenticates and encrypts the phone connection. Neither is the daemon token or the relay registration key.

The pairing URL is `https://fintwind-mobile.roketskiy.top/#v=1&route=TOKEN&key=SECRET`. Values use unpadded base64url. The phone removes the fragment after loading it and imports the secret into a non-extractable HKDF CryptoKey. IndexedDB retains the key and route for reopening the same browser; legacy per-tab plaintext storage is migrated and removed. No third-party script receives the values. The key is still usable by same-origin scripts; non-extractability is not an XSS defense. Closing the desktop, stopping remote control or re-pairing invalidates the old activation. Forgetting the computer removes browser authorization; it does not revoke a separate browser's authorization.

The phone opens `/c/TOKEN/__fintwind_remote__` through WSS and sends `{"type":"hello","version":1,"nonce":"BASE64URL"}` with a fresh 32-byte random nonce. The desktop replies with the same shape and its own fresh 32-byte nonce. Each WebSocket gets new nonces.

Derive separate 32-byte keys using HKDF-SHA256. The input key material is the pairing secret. The salt is the concatenation of the phone nonce and desktop nonce. The info strings are `fintwind.remote.v1.client` and `fintwind.remote.v1.server` for the respective sending directions.

Encrypted envelopes have `{"type":"sealed","seq":0,"data":"BASE64URL"}`. Use AES-256-GCM with a 128-bit tag appended to the ciphertext. The 12-byte nonce is four zero bytes followed by the unsigned 64-bit big-endian sequence. AAD is the UTF-8 string `fintwind.remote.v1`. Each direction starts at zero and accepts exactly the next sequence. Refuse sequence overflow and values beyond JavaScript's safe integer range.

The desktop first sends encrypted `{"type":"ready"}`. The phone sends commands only after decrypting this proof. The desktop executes commands only after successful decryption. Invalid authentication or framing closes the virtual socket without disclosing details. This PSK protocol does not provide forward secrecy.

## Commands and responses

Requests have `{"id":"UUID","command":{...}}`. Commands use a `type` discriminator and camelCase field names.

Read-only requests may also include a top-level `ifRevision` containing a 64-character lowercase SHA256 revision from a previous response. It is invalid on mutation commands. Authorization and recency checks run before computing the response revision. An unchanged response returns `{notModified:true,revision}`; otherwise the original response object includes `revision`. The phone sends a conditional request only after receiving a revision, so it remains compatible with older desktops. Its bounded in-memory cache separates pairing routes and resources and never caches mutation results.

| Type | Fields |
| --- | --- |
| `listWorkspaces` | None |
| `listSessions` | `projectId` |
| `getSession` | `sessionId` |
| `watchSession` | `sessionId`, `watchId` (UUID), `enabled` (read-only; requires `sessionDeltas`) |
| `getComposer` | `sessionId` (read-only; requires `composerOptions`) |
| `configureSession` | `sessionId`, optional `model`, `interactionMode`, `reasoningEffort` |
| `uploadChunk` | `sessionId`, `uploadId`, `name`, `offset`, `dataBase64`, `complete` |
| `createSession` | `projectId` |
| `sendPrompt` | `sessionId`, `text`, optional `attachmentIds` |
| `stop` | `sessionId` |
| `respondPermission` | `sessionId`, `requestId`, `optionId` |
| `answerQuestions` | `sessionId`, `requestId`, `answers` |

Each answer has `questionId` and an `answers` array of strings. The desktop validates answers against the current pending questions. Permission responses must match the current pending permission and one of its options.

Responses have `{"id":"UUID","ok":true,"data":...}` or `{"id":"UUID","ok":false,"error":"MESSAGE"}`. A successful mutation means the desktop accepted the operation, not that the Agent completed it. The phone never automatically retries a mutation. Mutation IDs remain reserved for the whole activation. A full deduplication table rejects new mutations rather than evicting old identities.

The phone keeps unresolved operation identities in route-scoped local storage, without message bodies. Before sending a mutation it must persist its identity. After a timeout, disconnect, page refresh or browser reopen, another mutation for that session or workspace stays blocked until a later successful read refreshes its current state. Web Locks allow only one active page for a pairing in the same browser. Unsupported lock/storage capabilities fail closed. This does not merge separate user actions merely because their text is identical.

## Response data

`listWorkspaces` returns `{workspaces:[{id,name}],selectedProjectId}`. Only activation-scoped projects that still exist are included. `listSessions` returns `{sessions:[{id,projectId,title,status,updatedAt}],cutoff}`. The cutoff is the current time minus 72 hours. Every read and mutation rechecks project membership and session recency.

`getSession` returns `{id,projectId,title,status,messages,truncated,permission,questions,desktopRequired,canSend}`. Each legacy message is `{id,role,text}`. Roles are `user` or `assistant`. Only a bounded tail is included. New desktops also return `features:["orderedTranscript","composerOptions","attachments"]`, `entries`, `model`, `modelName`, `interactionMode`, and `reasoningEffort`. Clients must not send the new commands to a desktop that does not advertise them.

`entries` interleaves `{type:"message",id,role,text,attachments:[{name,isImage}]}` and `{type:"activity",id,kind,title,text,complete,failed,truncated}` in transcript order. Tools carry only their name and status, never their arguments, output or target details. Only reasoning entries carry bounded thinking text. The list is capped at 200 entries and a 128 KiB serialized budget. Private control events, environment values and full daemon objects are excluded. The read adapter does not open files or transfer attachment bodies. Oversized approval or question forms set `desktopRequired` instead of presenting an incomplete form. A live session without hydrated history returns its available messages with `canSend:false`; after it becomes idle, hydration waits for a matching stored event cursor before replacing the incomplete history.

Message projections may include `createdAt` (Unix seconds) and `elapsedSeconds`. The terminal assistant part of a settled turn uses its real completion timestamp and the checked difference between completion and start; intermediate parts do not repeat the turn duration. Other message timestamps come from the stored message record. Missing or zero timestamps are omitted by the client, never replaced with the browser's current time. Duration zero remains a valid measured value. This is wall-clock turn duration, not token streaming time or throughput. Older desktops without these fields remain readable without a fabricated footer.

Desktops advertising `todoList` also return `todos:null` or
`{items:[{content,status}],completed,total,truncated}`. This is the newest valid
plan snapshot from the same parser used by the desktop capsule, independent of
the bounded transcript tail. An empty latest list clears the capsule. Status is
`pending`, `in_progress`, `completed` or `cancelled`. At most 64 items are sent,
with each content bounded to 512 UTF-8 bytes; counts describe the full plan and
`truncated` marks any omitted items or text. No raw tool arguments or outputs are
exposed. Session deltas carry these fields through the existing validation path.
Old desktops omit the field; clients show no capsule rather than invent a plan.
The capsule is read-only and floats above the composer. Scrollable bottom
padding lets the last transcript row move above the collapsed pill. Opening the
plan does not add padding, reduce the viewport or adjust the scroll position.

`workspaceChanges` adds `workspaceChanges:null` or `{additions,deletions}` to the
session projection. Totals use the desktop's `InspectBranches` operation,
including tracked changes against HEAD and eligible untracked text files. The
adapter resolves the requested session's workspace, not desktop selection. It
performs IPC and Git work in the background, shares pending requests by path,
and bounds its cache and in-flight requests to eight paths. Active subscriptions
check at most once per five seconds; hidden or closed watches stop requesting
refreshes. Unchanged counts do not notify the desktop or produce a new wire
snapshot. Activation and current workspace checks reject late results. Unknown,
failed and non-Git results are null, never fabricated zero counts. No repository
paths, file names or diff bodies are transferred.

`getComposer` returns `{models:[{id,name,provider,efforts:[{id,label}]}],commands:[{name,description}],canConfigure}`. Discovery runs off the UI thread for the target workspace, without changing desktop selection. Configuration validates model and effort against that workspace's catalog, refuses busy/preparing sessions and leaves permission levels unchanged.

OpenCode sessions may additionally advertise `goals` and return `goal:null` or `{objective,status,turns,maxTurns,reason,result,elapsedSeconds,tools}`. Status is `active`, `paused`, `blocked` or `done`. Objective and result are bounded to 8 KiB each, reason to 2 KiB. Turn counts are budgets, not a completion percentage. `sendPrompt.text` uses the existing desktop `/goal` command: no argument gets status; `pause`, `resume`, `clear` control it; any other argument starts an objective. Control commands reject attachments. Goal actions use the existing desktop control path and pending-preparation checks, not a new remote protocol command. Clients only offer Goal when `goals` is advertised.

Slow composer discovery and final attachment imports have independent bounded requests (at most eight). Their responses may arrive out of request order, with IDs used for matching. Encryption sequences are still emitted by one transport loop in strict order. These operations do not block status handling or stop requests. Ordinary mutations retain the existing serialized request path. Catalogs are cleared when pairing stops; remote model metadata has a 60-second lifetime and composer selections a 30-second lifetime, plus binary-generation and current-location catalog checks.

`uploadChunk` accepts standard base64, at most 65,536 characters per chunk. `offset` is the number of base64 characters already acknowledged; non-final chunks cannot contain padding. A complete file is at most 2 MiB decoded. At most 8 uploads and roughly 8 MiB decoded total are retained per activation for ten minutes. Completed uploads are imported into the daemon's attachment store on a background worker. The response is `{accepted:true,offset}`. `sendPrompt.attachmentIds` accepts only completed, same-session uploads from the current activation; no phone-provided paths or durable references are accepted. Attachment-only prompts are valid. Accepted sends consume these upload IDs; neither sends nor upload chunks are automatically retried. Unreferenced imported files use the daemon's existing cleanup policy.

`permission` is null or `{requestId,title,options:[{id,label,allow}]}`. `questions` is null or `{requestId,questions:[{id,header,question,multiSelect,options:[{label,description}]}]}`. `createSession` returns `{sessionId}`. Other mutations return `{accepted:true}`.

The phone refreshes bounded views on demand and while a task is open. It preserves the selected workspace and task independently of desktop selection. Rendering untrusted text uses DOM text content, never HTML.

### Optional session updates

`sessionDeltas` opts into updates on the existing encrypted connection. Neither
handshake nor `ready` changes. An old client never receives updates, and a new
client uses the 2.5-second conditional read fallback with an old desktop.
`watchSession` returns `{accepted:true}`. One watch replaces the previous watch;
disabling only affects a matching `watchId`. It does not accept `ifRevision`.

The first encrypted push is `{type:"sessionUpdate",watchId,snapshot}`. This is
the bounded `getSession` projection with `revision` and an empty legacy
`messages` array; ordered `entries` are authoritative. Subsequent pushes are a
smaller full snapshot or `{type:"sessionUpdate",watchId,baseRevision,revision,edits}`.
Each edit has a string-array `path` and exactly one of `set` (replace a value),
`append` (text suffix), or `truncate` (shorten an array). Array paths use decimal
indices. Edits run in order and also replace the snapshot's revision. The client
checks the base revision, bounded paths/values, and the complete reconstructed
session through its existing parser. Invalid or missing bases trigger a fresh
read and subscription; a repeated failure falls back to conditional reads.

The desktop folds entity notifications for 200ms, without adding window
notifications or frames. Projection is bounded; hashing, diffing, serialization
and encryption of wire frames run on the transport thread. Active Goal clocks also sample at
approximately one second. A one-item snapshot channel bounds backpressure.
Unchanged projections send nothing. Errors use `{type:"sessionUpdate",watchId,error}`.
Every projection rechecks activation, project membership, and session recency.
Closing the socket or replacing/disabling the watch drops its receiver and stops
the producer. A hidden phone page disables its watch, pauses reads, and creates a
new watch and full baseline on return. Stale watch IDs cannot update its display.

## Desktop adapter boundary

The `fintwind-remote` crate exports the following interface. Transport and cryptographic state stay private.

```rust
pub struct RelayConfig;
impl RelayConfig {
    pub fn load_default() -> anyhow::Result<Self>;
}
pub enum RemoteStatus { Connecting, Waiting, Connected, Failed(String) }
pub enum RemoteEvent {
    Status(RemoteStatus), Request(DesktopRequest),
    WatchSession { session_id: uuid::Uuid, snapshots: smol::channel::Sender<Result<serde_json::Value, String>> },
}
pub struct DesktopRequest {
    pub id: uuid::Uuid,
    pub command: RemoteCommand,
    pub respond: smol::channel::Sender<Result<serde_json::Value, String>>,
}
pub struct RemoteControl;
impl RemoteControl {
    pub fn start(config: RelayConfig) -> anyhow::Result<(Self, smol::channel::Receiver<RemoteEvent>)>;
    pub fn pairing_url(&self) -> &str;
    pub fn stop(&self);
}
```

`RemoteCommand` follows the command table, with snake_case Rust field names and UUID IDs. `RemoteAnswer` has `question_id: String` and `answers: Vec<String>`. Registration uses the installation identity in `~/.fintwind/remote-control-identity.bin`. The private key signs a challenge to obtain a short-lived JWT, which stays in memory. Windows protects the key with current-user DPAPI; Unix stores it in a user-only file. A damaged or revoked identity is never automatically replaced. The optional legacy `~/.fintwind/remote-control.json` is read on activation only to validate the fixed relay origin. Its old `registration_key` field is ignored. Neither installation keys nor JWTs enter pairing URLs, remote events or logs. See `remote-multiuser.md` for the admission contract.

Desktop callbacks capture the activation generation. Stopping increments that generation before dropping the control handle, so queued requests from an old activation cannot execute after revocation. Each request rechecks the live authorization before performing its action.
