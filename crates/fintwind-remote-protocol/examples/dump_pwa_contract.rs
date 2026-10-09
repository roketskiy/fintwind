//! Emit the exact browser contract and its negotiated protocol version.
//! Consumers generate types from these schemas; no Rust-source parsing.

fn main() {
    use fintwind_remote_protocol::{Frame, PairingInvite, SealedMessage, REMOTE_PROTOCOL_VERSION};

    let contract = serde_json::json!({
        "protocolVersion": REMOTE_PROTOCOL_VERSION,
        "schemas": {
            "outer": fintwind_remote_protocol::schema::wire_schema::<Frame>(),
            "inner": fintwind_remote_protocol::schema::wire_schema::<SealedMessage>(),
            "pairing": fintwind_remote_protocol::schema::wire_schema::<PairingInvite>(),
        }
    });
    println!("{}", serde_json::to_string_pretty(&contract).expect("contract serializes"));
}
