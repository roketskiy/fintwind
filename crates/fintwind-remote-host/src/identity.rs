//! Host identity: signing key (relay authentication) and Noise identity
//! (end-to-end channel), plus first-run registration.
//!
//! No enrollment form and no password exist anywhere (decision revision,
//! M0 freeze document): on first start the host generates its key pairs,
//! fetches a one-time challenge from the relay, signs it, and registers
//! its public key. The relay answers with a routing id that the host
//! stores beside its keys. Losing the data directory means a new identity
//! and a fresh pairing — there is nothing to "recover" by design.

use std::fs;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Serialize};

use fintwind_remote_crypto::{IdentityKey, SigningKey};

const SIGNING_KEY_FILE: &str = "host-signing-key.bin";
const NOISE_KEY_FILE: &str = "host-noise-key.bin";
const HOST_ID_FILE: &str = "host-id.txt";

pub struct HostIdentity {
    pub signing: SigningKey,
    pub noise: IdentityKey,
    pub host_id: String,
}

/// Load or create the persistent identity files (mode 0600 on creation;
/// DPAPI / credential-store integration lands with the pairing milestone
/// and is recorded as a follow-up in the plan).
pub fn load_or_create(data_dir: &Path) -> anyhow::Result<HostIdentity> {
    fs::create_dir_all(data_dir)?;
    let signing = load_or_create_signing(&data_dir.join(SIGNING_KEY_FILE))?;
    let noise = load_or_create_noise(&data_dir.join(NOISE_KEY_FILE))?;
    let host_id = match fs::read_to_string(data_dir.join(HOST_ID_FILE)) {
        Ok(id) => id.trim().to_owned(),
        Err(_) => String::new(),
    };
    Ok(HostIdentity {
        signing,
        noise,
        host_id,
    })
}

pub fn store_host_id(data_dir: &Path, host_id: &str) -> anyhow::Result<()> {
    fs::write(data_dir.join(HOST_ID_FILE), host_id)?;
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn load_or_create_signing(path: &Path) -> anyhow::Result<SigningKey> {
    match fs::read(path) {
        Ok(bytes) => SigningKey::from_bytes(bytes)
            .map_err(|error| anyhow::anyhow!("stored signing key unreadable: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let key = SigningKey::generate();
            write_private(path, &key.signing)?;
            Ok(key)
        }
        Err(error) => Err(error.into()),
    }
}

fn load_or_create_noise(path: &Path) -> anyhow::Result<IdentityKey> {
    match fs::read(path) {
        Ok(bytes) if bytes.len() == 64 => IdentityKey::from_bytes(
            bytes[..32].to_vec(),
            bytes[32..].to_vec(),
        )
        .map_err(|error| anyhow::anyhow!("stored noise key unreadable: {error}")),
        Ok(bytes) => anyhow::bail!(
            "stored noise key has the wrong length: {} bytes",
            bytes.len()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let key = IdentityKey::generate()?;
            let mut blob = key.public.clone();
            blob.extend_from_slice(&key.private);
            write_private(path, &blob)?;
            Ok(key)
        }
        Err(error) => Err(error.into()),
    }
}

/// Register this host's signing key with the relay (first start only; a
/// stored host id short-circuits). Proves possession by signing the
/// relay's one-time challenge.
pub fn register_with_relay(
    relay_origin: &str,
    signing: &SigningKey,
) -> anyhow::Result<String> {
    let origin = relay_origin.trim_end_matches('/');
    let challenge = post_json(
        &format!("{origin}/api/challenge"),
        &serde_json::json!({ "role": "host" }),
    )?;
    let challenge_b64 = challenge
        .get("challenge")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("relay challenge missing"))?
        .to_owned();
    let challenge_bytes = BASE64_STANDARD.decode(&challenge_b64)?;
    let signature = signing.sign(&challenge_bytes);
    let registered = post_json(
        &format!("{origin}/api/register"),
        &serde_json::json!({
            "role": "host",
            "signingPublicKey": BASE64_STANDARD.encode(&signing.verifying),
            "challenge": challenge_b64,
            "signature": BASE64_STANDARD.encode(signature),
        }),
    )?;
    registered
        .get("hostId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("relay registration returned no host id"))
}

use serde_json::Value;

/// Fetch a fresh one-time challenge for WebSocket authentication. Runs on
/// every (re)connect, just before the Auth frame.
pub fn fetch_challenge(relay_origin: &str, role: &str) -> anyhow::Result<(String, Vec<u8>)> {
    let origin = relay_origin.trim_end_matches('/');
    let challenge = post_json(
        &format!("{origin}/api/challenge"),
        &serde_json::json!({ "role": role }),
    )?;
    let challenge_b64 = challenge
        .get("challenge")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("relay challenge missing"))?
        .to_owned();
    let bytes = BASE64_STANDARD.decode(&challenge_b64)?;
    Ok((challenge_b64, bytes))
}

pub(crate) fn post_json(url: &str, body: &Value) -> anyhow::Result<Value> {
    let body = serde_json::to_string(body)?;
    // Plain http speaks hand-rolled HTTP/1.1 over TCP — loopback, tests,
    // source-site deployments. https keeps the system curl: the public
    // relay lives behind Cloudflare, and Schannel's trust store beats
    // rolling our own TLS client for a once-per-identity call.
    if let Some(rest) = url.strip_prefix("http://") {
        return post_json_plain(rest, &body);
    }
    let output = std::process::Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "15",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
            url,
        ])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("relay request failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| anyhow::anyhow!("relay response unreadable: {error}"))
}

/// Minimal HTTP/1.1 POST for plain-http origins. `rest` is everything
/// after `http://` (host[:port][/path]); the relay answers with
/// `Connection: close`, so reading to EOF is the whole response.
fn post_json_plain(rest: &str, body: &str) -> anyhow::Result<Value> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_owned()),
    };
    let mut stream = TcpStream::connect(authority)
        .map_err(|error| anyhow::anyhow!("relay connect failed: {error}"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(15)))?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let payload = parts.next().unwrap_or_default();
    let status = head.lines().next().unwrap_or_default();
    anyhow::ensure!(
        status.contains(" 200 "),
        "relay refused: {status} {}",
        payload.trim()
    );
    serde_json::from_str(payload)
        .map_err(|error| anyhow::anyhow!("relay response unreadable: {error}"))
}

/// The persisted pairing grant file shape is shared with grants.rs; this
/// helper exists for tests and tooling.
#[derive(Serialize, Deserialize)]
struct GrantFileShape {
    grants: Vec<serde_json::Value>,
}

#[allow(dead_code)]
fn _shape_witness(_: GrantFileShape) {}
