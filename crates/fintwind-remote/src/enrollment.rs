use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;

use crate::{RemoteFailure, crypto, identity::Identity};

pub(crate) const RELAY_ORIGIN: &str = "https://fintwind-mobile.roketskiy.top";
const ADMISSION_PREFIX: &str = "fintwind.relay.admission.v1";
const REQUEST_TIMEOUT_SECS: u64 = 15;
const RESPONSE_LIMIT: usize = 16 * 1024;
const JWT_LIMIT: usize = 4 * 1024;
const REFRESH_MARGIN_SECS: u64 = 300;
const MIN_CACHE_SECS: u64 = 30;
const DEFAULT_RATE_LIMIT_SECS: u64 = 30;
const MAX_RATE_LIMIT_SECS: u64 = 300;

/// Short-lived admission token plus the installation key that signs for it.
/// Neither value is formatted or logged.
pub(crate) struct CredentialCache {
    identity: Option<Identity>,
    token: Option<CachedToken>,
}

struct CachedToken {
    jwt: String,
    refresh_at: u64,
}

impl CredentialCache {
    pub(crate) fn new() -> Self {
        Self {
            identity: None,
            token: None,
        }
    }

    /// Drops the token but keeps the installation identity, which the server
    /// may have revoked and must not be silently replaced.
    pub(crate) fn invalidate(&mut self) {
        self.token = None;
    }

    pub(crate) fn token(&mut self) -> anyhow::Result<&str> {
        let reusable = self
            .token
            .as_ref()
            .is_some_and(|token| unix_now() < token.refresh_at);
        if !reusable {
            self.token = None;
            if self.identity.is_none() {
                self.identity = Some(
                    Identity::load_or_create()
                        .map_err(|_| failure(RemoteFailure::IdentityUnavailable))?,
                );
            }
            let identity = self
                .identity
                .as_ref()
                .context("installation identity is missing")?;
            self.token = Some(request_token(identity)?);
        }
        self.token
            .as_ref()
            .map(|token| token.jwt.as_str())
            .context("admission token is missing")
    }
}

fn request_token(identity: &Identity) -> anyhow::Result<CachedToken> {
    let public_key = identity.public_key();
    let challenge: Challenge = decode(
        post(
            "/v1/admission/challenge",
            &serde_json::json!({ "publicKey": &public_key }).to_string(),
        )?,
        false,
    )?;
    let nonce = decode_nonce(&challenge.nonce)?;
    let now = unix_now();
    if challenge.expires_at <= now || challenge.expires_at > now.saturating_add(90) {
        return Err(failure(RemoteFailure::Enrollment));
    }
    let nonce = crypto::encode(&nonce);
    let message = format!("{ADMISSION_PREFIX}\n{RELAY_ORIGIN}\n{nonce}\n{public_key}");
    let signature = crypto::encode(&identity.sign(message.as_bytes()));
    let token: TokenResponse = decode(
        post(
            "/v1/admission/token",
            &serde_json::json!({
                "publicKey": &public_key,
                "nonce": nonce,
                "signature": signature,
            })
            .to_string(),
        )?,
        true,
    )?;
    if token.installation_id != identity.installation_id() || token.token.len() > JWT_LIMIT {
        return Err(failure(RemoteFailure::Enrollment));
    }
    let expires_at = jwt_expiry(&token.token, identity.installation_id())?;
    if expires_at != token.expires_at {
        return Err(failure(RemoteFailure::Enrollment));
    }
    Ok(CachedToken {
        jwt: token.token,
        refresh_at: expires_at
            .saturating_sub(REFRESH_MARGIN_SECS)
            .max(unix_now() + MIN_CACHE_SECS),
    })
}

fn post(path: &str, body: &str) -> anyhow::Result<(u16, String, Option<u64>)> {
    let url = format!("{RELAY_ORIGIN}{path}");
    let headers = vec![
        "content-type: application/json".to_owned(),
        "accept: application/json".to_owned(),
    ];
    fintwind_protocol::http::http_json("POST", &url, &headers, Some(body), REQUEST_TIMEOUT_SECS)
        .map_err(|_| failure(RemoteFailure::Enrollment))
}

fn decode<T: serde::de::DeserializeOwned>(
    response: (u16, String, Option<u64>),
    token_step: bool,
) -> anyhow::Result<T> {
    let (status, body, retry_after) = response;
    match status {
        200 => {}
        // The relay refuses a revoked install at token issue, and refuses a
        // disabled public admission at challenge issue.
        403 if token_step => return Err(failure(RemoteFailure::Revoked)),
        403 => return Err(failure(RemoteFailure::Enrollment)),
        429 => {
            return Err(failure(RemoteFailure::RateLimited {
                retry_after: Duration::from_secs(
                    retry_after
                        .unwrap_or(DEFAULT_RATE_LIMIT_SECS)
                        .clamp(1, MAX_RATE_LIMIT_SECS),
                ),
            }));
        }
        _ => return Err(failure(RemoteFailure::Enrollment)),
    }
    if body.len() > RESPONSE_LIMIT {
        return Err(failure(RemoteFailure::Enrollment));
    }
    serde_json::from_str(&body).map_err(|_| failure(RemoteFailure::Enrollment))
}

fn decode_nonce(text: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| failure(RemoteFailure::Enrollment))?;
    let nonce: [u8; 32] = bytes
        .try_into()
        .map_err(|_| failure(RemoteFailure::Enrollment))?;
    if crypto::encode(&nonce) != text {
        return Err(failure(RemoteFailure::Enrollment));
    }
    Ok(nonce)
}

fn jwt_expiry(token: &str, installation_id: &str) -> anyhow::Result<u64> {
    let mut parts = token.split('.');
    let (Some(_header), Some(payload), Some(_signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(failure(RemoteFailure::Enrollment));
    };
    if payload.len() > JWT_LIMIT {
        return Err(failure(RemoteFailure::Enrollment));
    }
    let claims: Claims = URL_SAFE_NO_PAD
        .decode(payload)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| failure(RemoteFailure::Enrollment))?;
    if claims.sub != installation_id || claims.iss != "fintwind-relay" || claims.aud != "alma-relay"
    {
        return Err(failure(RemoteFailure::Enrollment));
    }
    let now = unix_now();
    if claims.exp <= now.saturating_add(MIN_CACHE_SECS)
        || claims.exp <= claims.iat
        || claims.iat > now.saturating_add(60)
        || claims.exp.saturating_sub(claims.iat) > 86460
    {
        return Err(failure(RemoteFailure::Enrollment));
    }
    Ok(claims.exp)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn failure(reason: RemoteFailure) -> anyhow::Error {
    anyhow::Error::new(reason)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Challenge {
    nonce: String,
    expires_at: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenResponse {
    token: String,
    installation_id: String,
    expires_at: u64,
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    exp: u64,
    iat: u64,
    iss: String,
    aud: String,
}
