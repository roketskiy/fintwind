use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    aead, hkdf,
    rand::{SecureRandom, SystemRandom},
};

const AAD: &[u8] = b"fintwind.remote.v1";
const MAX_SAFE_SEQ: u64 = 9_007_199_254_740_991;

pub(crate) fn random_32() -> anyhow::Result<[u8; 32]> {
    let mut value = [0; 32];
    SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| anyhow::anyhow!("secure random generation failed"))?;
    Ok(value)
}

pub(crate) fn encode(value: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(value)
}

pub(crate) struct Direction {
    key: aead::LessSafeKey,
    next: u64,
}

impl Direction {
    fn new(key: [u8; 32]) -> anyhow::Result<Self> {
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, &key)
            .map_err(|_| anyhow::anyhow!("invalid encryption key"))?;
        Ok(Self {
            key: aead::LessSafeKey::new(key),
            next: 0,
        })
    }

    fn nonce(seq: u64) -> aead::Nonce {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&seq.to_be_bytes());
        aead::Nonce::assume_unique_for_key(nonce)
    }

    pub(crate) fn seal(&mut self, plain: &[u8]) -> anyhow::Result<(u64, Vec<u8>)> {
        if self.next > MAX_SAFE_SEQ {
            bail!("sequence exhausted");
        }
        let seq = self.next;
        let mut encrypted = plain.to_vec();
        self.key
            .seal_in_place_append_tag(Self::nonce(seq), aead::Aad::from(AAD), &mut encrypted)
            .map_err(|_| anyhow::anyhow!("encryption failed"))?;
        self.next = self.next.checked_add(1).context("sequence exhausted")?;
        Ok((seq, encrypted))
    }

    pub(crate) fn open(&mut self, seq: u64, encrypted: &[u8]) -> anyhow::Result<Vec<u8>> {
        if seq != self.next || seq > MAX_SAFE_SEQ {
            bail!("invalid sequence");
        }
        let mut bytes = encrypted.to_vec();
        let plain = self
            .key
            .open_in_place(Self::nonce(seq), aead::Aad::from(AAD), &mut bytes)
            .map_err(|_| anyhow::anyhow!("invalid authentication"))?;
        let result = plain.to_vec();
        self.next = self.next.checked_add(1).context("sequence exhausted")?;
        Ok(result)
    }
}

pub(crate) fn directions(
    secret: &[u8; 32],
    phone_nonce: &[u8; 32],
    desktop_nonce: &[u8; 32],
) -> anyhow::Result<(Direction, Direction)> {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(phone_nonce);
    salt[32..].copy_from_slice(desktop_nonce);
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, &salt);
    let prk = salt.extract(secret);
    let client = prk
        .expand(&[b"fintwind.remote.v1.client"], hkdf::HKDF_SHA256)
        .map_err(|_| anyhow::anyhow!("key derivation failed"))?;
    let server = prk
        .expand(&[b"fintwind.remote.v1.server"], hkdf::HKDF_SHA256)
        .map_err(|_| anyhow::anyhow!("key derivation failed"))?;
    let mut client_key = [0u8; 32];
    let mut server_key = [0u8; 32];
    client
        .fill(&mut client_key)
        .map_err(|_| anyhow::anyhow!("key derivation failed"))?;
    server
        .fill(&mut server_key)
        .map_err(|_| anyhow::anyhow!("key derivation failed"))?;
    Ok((Direction::new(server_key)?, Direction::new(client_key)?))
}

/// One AES-GCM direction for the v2 signaling channel. The AAD binds every
/// sealed message to this activation's route, connection attempt, and sender
/// role, so a captured frame cannot be replayed into another context.
pub(crate) struct SignalDirection {
    key: aead::LessSafeKey,
    aad: Vec<u8>,
    next: u64,
}

impl SignalDirection {
    fn new(key: [u8; 32], aad: Vec<u8>) -> anyhow::Result<Self> {
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, &key)
            .map_err(|_| anyhow::anyhow!("invalid encryption key"))?;
        Ok(Self {
            key: aead::LessSafeKey::new(key),
            aad,
            next: 0,
        })
    }

    fn nonce(seq: u64) -> aead::Nonce {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&seq.to_be_bytes());
        aead::Nonce::assume_unique_for_key(nonce)
    }

    pub(crate) fn seal(&mut self, plain: &[u8]) -> anyhow::Result<(u64, Vec<u8>)> {
        if self.next > MAX_SAFE_SEQ {
            bail!("sequence exhausted");
        }
        let seq = self.next;
        let mut encrypted = plain.to_vec();
        self.key
            .seal_in_place_append_tag(
                Self::nonce(seq),
                aead::Aad::from(self.aad.as_slice()),
                &mut encrypted,
            )
            .map_err(|_| anyhow::anyhow!("encryption failed"))?;
        self.next = self.next.checked_add(1).context("sequence exhausted")?;
        Ok((seq, encrypted))
    }

    pub(crate) fn open(&mut self, seq: u64, encrypted: &[u8]) -> anyhow::Result<Vec<u8>> {
        if seq != self.next || seq > MAX_SAFE_SEQ {
            bail!("invalid sequence");
        }
        let mut bytes = encrypted.to_vec();
        let plain = self
            .key
            .open_in_place(
                Self::nonce(seq),
                aead::Aad::from(self.aad.as_slice()),
                &mut bytes,
            )
            .map_err(|_| anyhow::anyhow!("invalid authentication"))?;
        let result = plain.to_vec();
        self.next = self.next.checked_add(1).context("sequence exhausted")?;
        Ok(result)
    }
}

/// Derives the phone→desktop and desktop→phone signaling directions for one
/// connection attempt. Returns (outbound, inbound) from the desktop's view.
pub(crate) fn signal_directions(
    secret: &[u8; 32],
    route_hash: &str,
    attempt: &[u8; 32],
    attempt_text: &str,
) -> anyhow::Result<(SignalDirection, SignalDirection)> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, attempt);
    let prk = salt.extract(secret);
    let mut keys = [[0u8; 32]; 2];
    for (info, key) in [
        &b"fintwind.direct.v2.signal.desktop"[..],
        &b"fintwind.direct.v2.signal.phone"[..],
    ].into_iter().zip(keys.iter_mut()) {
        prk.expand(&[info], hkdf::HKDF_SHA256)
            .map_err(|_| anyhow::anyhow!("key derivation failed"))?
            .fill(key)
            .map_err(|_| anyhow::anyhow!("key derivation failed"))?;
    }
    let desktop_aad = format!("fintwind.direct.v2|{route_hash}|{attempt_text}|desktop").into_bytes();
    let phone_aad = format!("fintwind.direct.v2|{route_hash}|{attempt_text}|phone").into_bytes();
    Ok((
        SignalDirection::new(keys[0], desktop_aad)?,
        SignalDirection::new(keys[1], phone_aad)?,
    ))
}

pub(crate) fn sha256_hex(value: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, value)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
