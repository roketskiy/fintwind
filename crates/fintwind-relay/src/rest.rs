//! The relay's REST endpoints: one-time challenges, identity registration,
//! health. There are no accounts and no passwords (decision revision,
//! M0 freeze document): identity is an Ed25519 key pair, registration
//! proves possession with a signature over a fresh challenge, and every
//! later connection re-proves possession the same way over the WebSocket.
//!
//! Registration is open by design — a stranger registering a device is a
//! harmless empty shell that no host has ever paired with. What limits
//! abuse is the rate limiter and the one-time challenge consumption, not
//! a gate nobody would want to type a password through anyway.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::config::PAIRING_NONCE_TTL;
use crate::context::RelayContext;
use crate::http::RequestHead;
use fintwind_remote_crypto::SigningKey;

pub struct Response {
    pub status: u16,
    pub body: String,
}

fn ok(body: Value) -> Response {
    Response {
        status: 200,
        body: body.to_string(),
    }
}

fn error(status: u16, message: &str) -> Response {
    Response {
        status,
        body: json!({ "error": message }).to_string(),
    }
}

/// One-time challenges for registration and WebSocket authentication.
/// In memory by design: a relay restart voids outstanding challenges and
/// clients fetch a fresh one; they carry no state worth persisting.
pub struct ChallengeBook {
    challenges: Mutex<HashMap<[u8; 32], (ChallengeRole, Instant)>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChallengeRole {
    Host,
    Device,
}

impl ChallengeBook {
    pub fn new() -> Self {
        Self {
            challenges: Mutex::new(HashMap::new()),
        }
    }

    /// Issue a fresh challenge for a role. Valid for two minutes or until
    /// consumed, whichever comes first. Stored under its hash, so the
    /// book never holds the value it handed out.
    pub fn issue(&self, role: ChallengeRole) -> [u8; 32] {
        use rand::RngCore;
        use sha2::{Digest, Sha256};
        let mut challenge = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut challenge);
        let key: [u8; 32] = Sha256::digest(challenge).into();
        let mut challenges = self.challenges.lock();
        challenges.retain(|_, (_, expires)| *expires > Instant::now());
        challenges.insert(key, (role, Instant::now() + Duration::from_secs(120)));
        challenge
    }
    /// Consume a challenge: exactly one registration or authentication
    /// ever succeeds per issued challenge.
    pub fn consume(&self, challenge: &[u8], role: ChallengeRole) -> bool {
        use sha2::{Digest, Sha256};
        let key: [u8; 32] = Sha256::digest(challenge).into();
        let mut challenges = self.challenges.lock();
        matches!(challenges.remove(&key), Some((issued_role, expires)) if issued_role == role && expires > Instant::now())
    }
}

fn field<'a>(body: &'a Value, name: &str) -> anyhow::Result<&'a str> {
    body.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing {name}"))
}

fn decode(value: &str) -> anyhow::Result<Vec<u8>> {
    BASE64_STANDARD
        .decode(value)
        .map_err(|_| anyhow::anyhow!("bad base64"))
}

pub fn handle(context: &RelayContext, request: &RequestHead, body: &[u8]) -> Response {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/health") => ok(json!({ "ok": true })),
        ("POST", "/api/challenge") => post_json(context, body, challenge),
        ("POST", "/api/register") => post_json(context, body, register),
        ("POST", "/api/pairing/offer") => post_json(context, body, pairing_offer),
        _ => error(404, "not found"),
    }
}

fn post_json(
    context: &RelayContext,
    body: &[u8],
    handler: fn(&RelayContext, &Value) -> anyhow::Result<Response>,
) -> Response {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(parsed) => parsed,
        Err(_) => return error(400, "malformed json"),
    };
    match handler(context, &parsed) {
        Ok(response) => response,
        Err(_) => error(400, "request refused"),
    }
}

/// Issue a challenge for one role. Rate limited per process-wide attempt
/// budget so a flood cannot pre-allocate memory.
fn challenge(context: &RelayContext, body: &Value) -> anyhow::Result<Response> {
    let role = parse_role(body)?;
    if !context.auth_limiter.admit("challenges") {
        return Ok(error(429, "too many requests"));
    }
    let challenge = context.challenges.issue(role);
    Ok(ok(json!({ "challenge": BASE64_STANDARD.encode(challenge) })))
}

/// A host posts its pairing offer before displaying the QR: the relay
/// stores the nonce (hashed, TTL, atomically consumable) so exactly one
/// `LinkDevice` can ever land for this invite. Authenticated like every
/// other signed call: challenge, then signature.
fn pairing_offer(context: &RelayContext, body: &Value) -> anyhow::Result<Response> {
    let host_id = field(body, "hostId")?;
    let nonce = decode(field(body, "nonce")?)?;
    let challenge_bytes = decode(field(body, "challenge")?)?;
    let signature = decode(field(body, "signature")?)?;
    if nonce.is_empty() || signature.len() != 64 {
        return Ok(error(400, "bad nonce or signature length"));
    }
    if !context.challenges.consume(&challenge_bytes, ChallengeRole::Host) {
        return Ok(error(401, "challenge unknown or consumed"));
    }
    // Signature over the offer payload, keyed by the host's registered
    // signing key: only a live host can arm an invite.
    let mut message = host_id.as_bytes().to_vec();
    message.extend_from_slice(&nonce);
    let Some(host_key) = context.store.host_signing_key(host_id)? else {
        return Ok(error(401, "unknown host"));
    };
    if !SigningKey::verify(&host_key, &message, &signature) {
        return Ok(error(401, "signature verification failed"));
    }
    if !context.auth_limiter.admit("pairing-offers") {
        return Ok(error(429, "too many requests"));
    }
    context.store.store_pairing_nonce(host_id, &BASE64_STANDARD.encode(&nonce))?;
    log::info!(target: "audit", "pairing offer armed; host={host_id}");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    Ok(ok(json!({ "expiresAtMs": now + PAIRING_NONCE_TTL.as_millis() as u64 })))
}

/// Register an identity: public key + signature over a consumed challenge.
/// Re-registering a live key returns the existing id; a revoked key is
/// refused outright — replacement means a genuinely new key pair.
fn register(context: &RelayContext, body: &Value) -> anyhow::Result<Response> {
    let role = parse_role(body)?;
    let public_key = decode(field(body, "signingPublicKey")?)?;
    let challenge_bytes = decode(field(body, "challenge")?)?;
    let signature = decode(field(body, "signature")?)?;
    if public_key.len() != 32 || signature.len() != 64 {
        return Ok(error(400, "bad key or signature length"));
    }
    if !context.challenges.consume(&challenge_bytes, role) {
        return Ok(error(401, "challenge unknown or consumed"));
    }
    if !SigningKey::verify(&public_key, &challenge_bytes, &signature) {
        return Ok(error(401, "signature verification failed"));
    }
    if !context.auth_limiter.admit("register") {
        return Ok(error(429, "too many requests"));
    }
    match role {
        ChallengeRole::Host => {
            let host_id = context.store.register_host(&public_key)?;
            log::info!(target: "audit", "host registered; host={host_id}");
            Ok(ok(json!({ "hostId": host_id })))
        }
        ChallengeRole::Device => {
            let name = field(body, "name").unwrap_or("");
            let device_id = context.store.register_device(&public_key, name)?;
            log::info!(target: "audit", "device registered; device={device_id}");
            Ok(ok(json!({ "deviceId": device_id })))
        }
    }
}

fn parse_role(body: &Value) -> anyhow::Result<ChallengeRole> {
    match body.get("role").and_then(Value::as_str) {
        Some("host") => Ok(ChallengeRole::Host),
        Some("device") => Ok(ChallengeRole::Device),
        _ => Err(anyhow::anyhow!("missing role")),
    }
}
