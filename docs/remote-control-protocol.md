# Remote control protocol

This reference defines Fintwind remote protocol 1. It is not Alma's encryption protocol. Alma Relay only carries the outer tunnel frames. The desktop accepts `/__fintwind_remote__` WebSocket opens and rejects every HTTP request and other WebSocket path.

## Pairing and encryption

Every activation generates two independent 32-byte random values. The routing token identifies the tunnel. The pairing secret authenticates and encrypts the phone connection. Neither is the daemon token or the relay registration key.

The pairing URL is `https://fintwind-mobile.roketskiy.top/#v=1&route=TOKEN&key=SECRET`. Values use unpadded base64url. The phone removes the fragment after loading it. Per-tab session storage may retain the pairing values to permit page refresh. No third-party script receives the values.

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
| `createSession` | `projectId` |
| `sendPrompt` | `sessionId`, `text` |
| `stop` | `sessionId` |
| `respondPermission` | `sessionId`, `requestId`, `optionId` |
| `answerQuestions` | `sessionId`, `requestId`, `answers` |

Each answer has `questionId` and an `answers` array of strings. The desktop validates answers against the current pending questions. Permission responses must match the current pending permission and one of its options.

Responses have `{"id":"UUID","ok":true,"data":...}` or `{"id":"UUID","ok":false,"error":"MESSAGE"}`. A successful mutation means the desktop accepted the operation, not that the Agent completed it. The phone never automatically retries a mutation. Mutation IDs remain reserved for the whole activation. A full deduplication table rejects new mutations rather than evicting old identities.

The phone keeps unresolved operation identities in per-tab session storage. After a timeout, disconnect or page refresh, another mutation for that session or workspace stays blocked until a later successful read refreshes its current state. This does not merge separate user actions merely because their text is identical.

## Response data

`listWorkspaces` returns `{workspaces:[{id,name}],selectedProjectId}`. Only activation-scoped projects that still exist are included. `listSessions` returns `{sessions:[{id,projectId,title,status,updatedAt}],cutoff}`. The cutoff is the current time minus 72 hours. Every read and mutation rechecks project membership and session recency.

`getSession` returns `{id,projectId,title,status,messages,truncated,permission,questions,desktopRequired,canSend}`. Each message is `{id,role,text}`. Roles are `user` or `assistant`. Only a bounded tail of text is included. Internal control events, attachments, environment values and full daemon objects are excluded. The adapter does not read files or transfer attachment bodies. Oversized approval or question forms set `desktopRequired` instead of presenting an incomplete form. A live session without hydrated history returns its available messages with `canSend:false`; after it becomes idle, hydration waits for a matching stored event cursor before replacing the incomplete history.

`permission` is null or `{requestId,title,options:[{id,label,allow}]}`. `questions` is null or `{requestId,questions:[{id,header,question,multiSelect,options:[{label,description}]}]}`. `createSession` returns `{sessionId}`. Other mutations return `{accepted:true}`.

The phone refreshes bounded views on demand and while a task is open. It preserves the selected workspace and task independently of desktop selection. Rendering untrusted text uses DOM text content, never HTML.

## Desktop adapter boundary

The `fintwind-remote` crate exports the following interface. Transport and cryptographic state stay private.

```rust
pub struct RelayConfig;
impl RelayConfig {
    pub fn load_default() -> anyhow::Result<Self>;
}
pub enum RemoteStatus { Connecting, Waiting, Connected, Failed(String) }
pub enum RemoteEvent { Status(RemoteStatus), Request(DesktopRequest) }
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
