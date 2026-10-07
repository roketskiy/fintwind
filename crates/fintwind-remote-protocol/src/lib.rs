//! Remote-control wire contract for Fintwind, split into two layers.
//!
//! The outer [`frame`] layer is everything the relay sees: Noise handshake
//! messages, sealed ciphertext, and relay-level control semantics. The
//! inner [`message`] layer travels encrypted inside `Frame::Sealed` and is
//! opaque to the relay — business payloads, operations, approvals, and
//! event streams with cursor semantics.
//!
//! Semantics and safety requirements come from [remote-control-relay.md];
//! frozen decisions (Noise parameters, lifetimes, key periods) come from
//! [remote-control-m0-freeze.md]. Both layers carry the protocol version
//! explicitly, and a mismatch closes the connection: there is no
//! compatibility fallback. Unknown variants and unknown fields are
//! rejected at the serde layer, so an unrecognized message produces no
//! control effect.
//!
//! The schema module exports JSON Schema for every wire type so the PWA
//! can generate TypeScript definitions; CI compares that output against
//! the committed types.
//!
//! [remote-control-relay.md]: ../../docs/remote-control-relay.md
//! [remote-control-m0-freeze.md]: ../../docs/remote-control-m0-freeze.md

mod frame;
mod message;
pub mod schema;

pub use frame::{
    Base64Bytes, ControlFrame, Frame, FrameRole, HandshakeMode, PairingInvite, RejectCode,
    MAX_FRAME_BYTES, REMOTE_PROTOCOL_VERSION,
};
pub use message::{
    DriverEventFrame, MessageBody, OperationKind, OperationOutcome, OperationRequest,
    OperationState, PermissionDecision, PermissionOffer, PermissionOption, ResumeCursor,
    SealedMessage, SessionSummary, UserInputAnswer, UserInputDecision, UserInputOffer,
    UserInputQuestion,
};
