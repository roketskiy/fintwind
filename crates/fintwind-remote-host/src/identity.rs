//! Host identity: signing key (relay authentication) and Noise identity
//! (end-to-end channel), plus first-run registration.
//!
//! No enrollment form and no password exist anywhere (decision revision,
//! M0 freeze document): on first start the host generates its key pairs,
//! fetches a one-time challenge from the relay, signs it, and registers
//! its public key. The relay answers with a routing id that the host
//! stores beside its keys. Losing the data directory means a new identity
//! and a fresh pairing — there is nothing to "recover" by design.
//!
//! Secret storage (M0 freeze § 1: OS credential protection):
//!
//! - Windows: the private keys are sealed with DPAPI into `*.dpapi` files
//!   next to the identity. Only the same user on the same machine can open
//!   them.
//! - Everywhere else: the historical plaintext files with mode 0600.
//!
//! Migration from a plaintext file is deliberately ordered so an identity
//! can never be lost or silently replaced:
//!
//! 1. a sealed record that exists but fails to open is a **fatal error** —
//!    the identity is unreadable here (wrong user, wrong machine, tampered
//!    file) and must never fall back to generating a fresh key, and never
//!    fall back to whatever plaintext sits beside it;
//! 2. a plaintext file is migrated only after the sealed record is written
//!    **and** read back and round-tripped to the exact same bytes, and only
//!    then is the plaintext secret deleted;
//! 3. a failed seal keeps the plaintext file and degrades honestly — this
//!    tolerance exists **only** for a legacy plaintext identity that is
//!    already on disk. A *fresh* identity is durable or the process fails:
//!    there is no plaintext fallback and no silently short-lived identity
//!    that could register and pair and then turn into a different host.
//!
//! Loading, migration and fresh generation share an OS file lock. A
//! contender reads only after the winner finishes, and a crashed process
//! releases the lock automatically. The empty lock file stays in place:
//! removing it would let other processes lock two different file objects.
//! A half-stored identity is refused, never completed with a fresh half.

mod dpapi;

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Serialize};

use fintwind_remote_crypto::{IdentityKey, SigningKey};

const SIGNING_KEY_FILE: &str = "host-signing-key.bin";
const SIGNING_KEY_SEALED_FILE: &str = "host-signing-key.dpapi";
const NOISE_KEY_FILE: &str = "host-noise-key.bin";
const NOISE_KEY_SEALED_FILE: &str = "host-noise-key.dpapi";
const HOST_ID_FILE: &str = "host-id.txt";
const GENERATION_LOCK_FILE: &str = "host-identity.lock";

pub struct HostIdentity {
    pub signing: SigningKey,
    pub noise: IdentityKey,
    pub host_id: String,
}

/// Load or create the persistent identity files. A stored identity is
/// always loaded as-is — a corrupt, unreadable or foreign-key identity is
/// an error, never a reason to mint a new one.
pub fn load_or_create(data_dir: &Path) -> anyhow::Result<HostIdentity> {
    fs::create_dir_all(data_dir)?;
    let _identity_lock = acquire_identity_lock(data_dir)?;
    let signing = load_secret(
        data_dir,
        SIGNING_KEY_FILE,
        SIGNING_KEY_SEALED_FILE,
        "signing key",
    )?;
    let noise = load_secret(data_dir, NOISE_KEY_FILE, NOISE_KEY_SEALED_FILE, "noise key")?;
    let (signing, noise) = match (signing, noise) {
        (Some(signing), Some(noise)) => (signing, noise),
        // A half-stored identity is never completed with a fresh half:
        // a signing key from one generation and a noise key from another
        // is exactly the silent identity split the freeze forbids.
        (Some(_), None) | (None, Some(_)) => {
            bail!(
                "a partial stored identity exists (one key sealed, the other missing); \
                 no replacement half is generated; restore a complete stored \
                 identity or explicitly create a new host and re-pair"
            );
        }
        (None, None) => generate_identity(data_dir)?,
    };
    let signing = SigningKey::from_bytes(signing)
        .map_err(|error| anyhow::anyhow!("stored signing key unreadable: {error}"))?;
    ensure!(
        noise.len() == 64,
        "stored noise key has the wrong length: {} bytes",
        noise.len()
    );
    let noise = IdentityKey::from_bytes(noise[..32].to_vec(), noise[32..].to_vec())
        .map_err(|error| anyhow::anyhow!("stored noise key unreadable: {error}"))?;
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

/// Write `bytes` so a crash can never leave a half-written record: the
/// bytes land in a sibling temp file first and are renamed into place. A
/// failed rename removes the temp file, so a failed write leaves nothing
/// half-done behind.
fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    // A per-write unique temp name: two processes racing one record can
    // never interleave their bytes inside a single fixed sibling file.
    let unique = format!(
        "{}.tmp-{}",
        path.file_name().and_then(|name| name.to_str()).unwrap_or("record"),
        uuid::Uuid::new_v4(),
    );
    let temp = path.with_file_name(unique);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(error).context("could not flush the identity record");
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error).context("could not commit the identity record");
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Seal `secret` into `sealed_path`. The record is written atomically and
/// read back and opened once, so only a verified record is ever reported
/// as stored.
fn write_sealed(sealed_path: &Path, secret: &[u8]) -> anyhow::Result<()> {
    let blob = dpapi::protect(secret).context("could not seal the identity secret")?;
    write_atomic(sealed_path, &blob)?;
    let stored = fs::read(sealed_path)
        .with_context(|| format!("could not read back {}", sealed_path.display()))?;
    let opened = dpapi::unprotect(&stored)
        .with_context(|| format!("could not open {}", sealed_path.display()))?;
    ensure!(
        opened == secret,
        "the sealed record does not round-trip to the same secret"
    );
    Ok(())
}

/// Load one secret, migrating a plaintext file when the platform seals.
///
/// A present sealed record *is* the identity: any failure to read or open
/// it is fatal, and never a reason to adopt whatever plaintext sits beside
/// it (a planted file must not replace a stored identity). A directory
/// occupying the sealed name is not a record — that is the disclosed
/// blocked-write-target case, handled by the legacy migration below.
fn load_secret(
    data_dir: &Path,
    plain_name: &str,
    sealed_name: &str,
    what: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let sealed_path = data_dir.join(sealed_name);
    let plain_path = data_dir.join(plain_name);
    let sealed_exists = match fs::symlink_metadata(&sealed_path) {
        Ok(metadata) if metadata.is_file() => true,
        Ok(metadata) if metadata.is_dir() => false,
        Ok(_) => bail!("the sealed identity path is not a regular record"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("could not inspect the sealed identity record"),
    };
    if sealed_exists {
        let blob = fs::read(&sealed_path)
            .with_context(|| format!("could not read {}", sealed_path.display()))?;
        let secret = dpapi::unprotect(&blob).with_context(|| {
            format!(
                "the {what} is stored encrypted but cannot be opened on this machine \
                 (different user, different machine, or a tampered file). The identity \
                 cannot be read here; re-pairing after moving the data directory is the \
                 only path forward."
            )
        })?;
        return Ok(Some(secret));
    }
    match fs::read(&plain_path) {
        Ok(secret) => {
            #[cfg(windows)]
            match write_sealed(&sealed_path, &secret) {
                Ok(()) => {
                    log::info!(
                        target: "audit",
                        "{what} migrated to encrypted storage; file={sealed_name}"
                    );
                    // Only now, with a verified sealed record on disk, does
                    // the plaintext secret go away.
                    if let Err(error) = fs::remove_file(&plain_path) {
                        log::warn!(
                            target: "audit",
                            "sealed record stored but the plaintext {plain_name} could not be removed: {error}"
                        );
                    }
                }
                Err(error) => {
                    // Keep the plaintext secret and keep running: the
                    // identity is intact, only unprotected. The next start
                    // retries the migration.
                    log::warn!(
                        target: "audit",
                        "encrypted {what} storage unavailable; keeping {plain_name}: {error:#}"
                    );
                }
            }
            Ok(Some(secret))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("could not read {plain_name}")),
    }
}

/// Store a freshly generated secret: sealed and round-trip verified, or a
/// failure. A fresh secret has no plaintext fallback — an identity that is
/// not durably stored must never register, pair, or run, because it would
/// silently become a different host on the next start. (The plaintext
/// tolerance lives only in load_secret's legacy migration path.)
fn store_new_secret(
    data_dir: &Path,
    plain_name: &str,
    sealed_name: &str,
    secret: &[u8],
    what: &str,
) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let _ = plain_name;
    write_sealed(&data_dir.join(sealed_name), secret)
        .with_context(|| format!("could not durably seal the fresh {what}"))
    }
    #[cfg(not(windows))]
    {
        let _ = sealed_name;
        let path = data_dir.join(plain_name);
        write_atomic(&path, secret)
            .with_context(|| format!("could not store the fresh {what}"))?;
        ensure!(fs::read(&path)? == secret, "the identity record did not round-trip");
        Ok(())
    }
}

/// Generate and durably seal both identity secrets. Fresh generation is
/// serialized between processes and is durable or it fails.
fn generate_identity(data_dir: &Path) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let signing = SigningKey::generate();
    store_new_secret(
        data_dir,
        SIGNING_KEY_FILE,
        SIGNING_KEY_SEALED_FILE,
        &signing.signing,
        "signing key",
    )?;
    let noise = IdentityKey::generate()?;
    let mut blob = noise.public.clone();
    blob.extend_from_slice(&noise.private);
    store_new_secret(data_dir, NOISE_KEY_FILE, NOISE_KEY_SEALED_FILE, &blob, "noise key")?;
    Ok((signing.signing, blob))
}

/// The OS releases this lock when the handle closes, including on crash.
/// Keep the filename stable so all contenders lock the same file object.
fn acquire_identity_lock(data_dir: &Path) -> anyhow::Result<fs::File> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join(GENERATION_LOCK_FILE))
        .context("could not open the identity lock")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(error)) => return Err(error).context("could not lock the identity records"),
        }
        if Instant::now() >= deadline {
            bail!("another process still holds the identity lock");
        }
        std::thread::sleep(Duration::from_millis(25));
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
    let mut command = std::process::Command::new("curl");
    command.args([
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
    ]);
    #[cfg(windows)]
    {
        // This runs inside the GUI app during pairing: without this flag
        // every challenge fetch flashes a console window.
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command.output()?;
    if !output.status.success() {
        anyhow::bail!("relay request failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| anyhow::anyhow!("relay response unreadable: {error}"))?;
    // curl reports 4xx/5xx bodies with exit 0; the relay's own error
    // object is the only failure signal on the https path.
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        anyhow::bail!("relay refused {url}: {error}");
    }
    Ok(value)
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
