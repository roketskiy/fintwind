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
use serde_json::{Value, json};
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
    ///
    /// The book has a fixed outstanding budget: a local or proxy
    /// misconfiguration that keeps requesting challenges without ever
    /// completing a registration would otherwise grow it without bound.
    /// At the budget new requests are refused. Active challenges are never
    /// evicted to make room for a flood of new unauthenticated requests.
    pub fn issue(&self, role: ChallengeRole) -> Option<[u8; 32]> {
        use rand::RngCore;
        use sha2::{Digest, Sha256};
        let mut challenge = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut challenge);
        let key: [u8; 32] = Sha256::digest(challenge).into();
        let mut challenges = self.challenges.lock();
        let now = Instant::now();
        challenges.retain(|_, (_, expires)| *expires > now);
        if challenges.len() >= crate::config::MAX_OUTSTANDING_CHALLENGES
            && !challenges.contains_key(&key)
        {
            return None;
        }
        challenges.insert(key, (role, now + Duration::from_secs(120)));
        Some(challenge)
    }
    /// Consume a challenge: exactly one registration or authentication
    /// ever succeeds per issued challenge.
    pub fn consume(&self, challenge: &[u8], role: ChallengeRole) -> bool {
        use sha2::{Digest, Sha256};
        let key: [u8; 32] = Sha256::digest(challenge).into();
        let mut challenges = self.challenges.lock();
        let found = challenges.remove(&key);
        let ok = matches!(&found, Some((issued_role, expires)) if *issued_role == role && expires > &Instant::now());
        if !ok {
            log::warn!(target: "audit", "challenge consume failed; len={} found={} role_ok={:?}", challenge.len(), found.is_some(), found.as_ref().map(|(role, _)| role.clone()));
        }
        ok
    }
}

fn field<'a>(body: &'a Value, name: &str) -> anyhow::Result<&'a str> {
    body.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing {name}"))
}

/// The rate-limit key basis for a request. `X-Forwarded-For` is only
/// believed when the direct peer is the local trusted proxy; otherwise
/// the peer's own address is the key, so a client cannot spend someone
/// else's budget by writing a header.
/// The rate-limit key basis for a request.
///
/// Only a loopback peer can be the trusted proxy, and only when it hands
/// over **one** canonical client address. A multi-value or malformed
/// `X-Forwarded-For` is treated as no forwarding at all and falls back to
/// the peer's own address: the alternative is letting arbitrary strings
/// become rate-limit keys, which would spend the budget of real clients
/// (and grow the limiter's map) without ever being the client's own.
fn source_key(request: &RequestHead) -> String {
    if request.trusted_proxy {
        if let Some(forwarded) = request.header("x-forwarded-for") {
            if let Some(canonical) = canonical_client_ip(forwarded) {
                return canonical;
            }
        }
    }
    request
        .peer_ip
        .clone()
        .unwrap_or_else(|| "local".to_owned())
}

/// The single client address a trusted proxy reports, if it reports
/// exactly one and it parses. A chain that has accumulated more than one
/// entry, or a value that is not an IP at all, yields nothing: the caller
/// then keys on the proxy's own address, which is a safe shared bucket
/// rather than a client-chosen string.
fn canonical_client_ip(forwarded: &str) -> Option<String> {
    let mut entries = forwarded.split(',');
    let first = entries.next()?.trim();
    let single = entries.next().is_none();
    if !single || first.is_empty() {
        return None;
    }
    first
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| ip.to_string())
}

fn decode(value: &str) -> anyhow::Result<Vec<u8>> {
    BASE64_STANDARD
        .decode(value)
        .map_err(|_| anyhow::anyhow!("bad base64"))
}

pub fn handle(context: &RelayContext, request: &RequestHead, body: &[u8]) -> Response {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/health") => health(context),
        ("POST", "/api/challenge") => post_json(context, request, body, challenge),
        ("POST", "/api/register") => post_json(context, request, body, register),
        ("POST", "/api/pairing/offer") => post_json(context, request, body, pairing_offer),
        // Browser preflight for the PWA on an allowed origin. The CORS
        // headers themselves are added by the transport layer, which
        // knows the request's Origin and reflects only an allow-listed
        // one; a preflight from any other origin never reaches here.
        ("OPTIONS", _) => Response {
            status: 204,
            body: String::new(),
        },
        _ => error(404, "not found"),
    }
}

/// Health plus the minimal aggregate counters. Everything here is a
/// total or a depth: no subject, no connection, no payload. An operator
/// can see that the relay is refusing work and why in kind, never who.
fn health(context: &RelayContext) -> Response {
    let metrics = context.metrics.snapshot();
    let (queued_bytes, queued_messages, queue_devices) = context.registry.queue_totals();
    let mut rejections = serde_json::Map::new();
    for (name, count) in crate::metrics::REJECT_CODE_NAMES
        .iter()
        .zip(metrics.rejections.iter())
    {
        rejections.insert((*name).to_owned(), json!(count));
    }
    ok(json!({
        "ok": true,
        "sessions": {
            "authenticated": context.registry.connection_count(),
            "pending": context
                .pending_connections
                .load(std::sync::atomic::Ordering::Acquire),
        },
        "queue": {
            "messages": queued_messages,
            "bytes": queued_bytes,
            "devices": queue_devices,
        },
        "outbound": { "bytes": context.registry.outbound_total_bytes() },
        "totals": {
            "authAccepted": metrics.auth_accepted,
            "authRejected": metrics.auth_rejected,
            "framesRouted": metrics.frames_routed,
            "timeouts": metrics.timeouts,
            "unauthenticatedRefused": metrics.unauthenticated_refused,
            "rejections": metrics.rejections.iter().sum::<u64>(),
        },
        "rejectionsByCode": Value::Object(rejections),
    }))
}

fn post_json(
    context: &RelayContext,
    request: &RequestHead,
    body: &[u8],
    handler: fn(&RelayContext, &RequestHead, &Value) -> anyhow::Result<Response>,
) -> Response {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(parsed) => parsed,
        Err(_) => return error(400, "malformed json"),
    };
    match handler(context, request, &parsed) {
        Ok(response) => response,
        Err(_) => error(400, "request refused"),
    }
}

/// Issue a challenge for one role. Rate limited per source address: the
/// challenge budget exists to stop one flooder from pre-allocating
/// memory, and a host's legitimate reconnect backoff must never spend
/// every other client's budget (it did, relay-wide, before this bucket).
fn challenge(
    context: &RelayContext,
    request: &RequestHead,
    body: &Value,
) -> anyhow::Result<Response> {
    let role = parse_role(body)?;
    // Caddy fronts the relay locally; the caller's address rides in
    // X-Forwarded-For, direct callers fall back to one shared bucket.
    let source = source_key(request);
    if !context.auth_limiter.admit(&format!("challenges:{source}")) {
        return Ok(error(429, "too many requests"));
    }
    // The book's fixed outstanding budget is full: answer 503 rather
    // than grow. The endpoint retries; nothing else about the request
    // was wrong.
    let Some(challenge) = context.challenges.issue(role) else {
        context.metrics.unauthenticated_refused();
        return Ok(error(503, "challenge budget full"));
    };
    Ok(ok(
        json!({ "challenge": BASE64_STANDARD.encode(challenge) }),
    ))
}

/// A host posts its pairing offer before displaying the QR: the relay
/// stores the nonce (hashed, TTL, atomically consumable) so exactly one
/// `LinkDevice` can ever land for this invite. Authenticated like every
/// other signed call: challenge, then signature.
fn pairing_offer(
    context: &RelayContext,
    _request: &RequestHead,
    body: &Value,
) -> anyhow::Result<Response> {
    let host_id = field(body, "hostId")?;
    let nonce = decode(field(body, "nonce")?)?;
    let challenge_bytes = decode(field(body, "challenge")?)?;
    let signature = decode(field(body, "signature")?)?;
    if nonce.is_empty() || signature.len() != 64 {
        return Ok(error(400, "bad nonce or signature length"));
    }
    if !context
        .challenges
        .consume(&challenge_bytes, ChallengeRole::Host)
    {
        return Ok(error(401, "challenge unknown or consumed"));
    }
    // Signature over the offer payload, keyed by the host's registered
    // signing key: only a live host can arm an invite. The signature is
    // what makes `host_id` trustworthy, so the rate limit below keys on
    // it — one flooder can no longer spend every other host's budget.
    let mut message = host_id.as_bytes().to_vec();
    message.extend_from_slice(&nonce);
    let Some(host_key) = context.store.host_signing_key(host_id)? else {
        return Ok(error(401, "unknown host"));
    };
    if !SigningKey::verify(&host_key, &message, &signature) {
        return Ok(error(401, "signature verification failed"));
    }
    if !context
        .auth_limiter
        .admit(&format!("pairing-offers:{host_id}"))
    {
        return Ok(error(429, "too many requests"));
    }
    context
        .store
        .store_pairing_nonce(host_id, &BASE64_STANDARD.encode(&nonce))?;
    log::info!(target: "audit", "pairing offer armed; host={host_id}");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    Ok(ok(
        json!({ "expiresAtMs": now + PAIRING_NONCE_TTL.as_millis() as u64 }),
    ))
}

/// Register an identity: public key + signature over a consumed challenge.
/// Re-registering a live key returns the existing id; a revoked key is
/// refused outright — replacement means a genuinely new key pair.
fn register(
    context: &RelayContext,
    request: &RequestHead,
    body: &Value,
) -> anyhow::Result<Response> {
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
    // The registrant is not an authenticated identity yet, so the budget
    // keys on the source address — the same per-source shape the
    // challenge endpoint uses, instead of one global bucket a single
    // flooder can empty for everyone.
    let source = source_key(request);
    if !context.auth_limiter.admit(&format!("register:{source}")) {
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
