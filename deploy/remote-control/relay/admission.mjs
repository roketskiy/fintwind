// Persistent-install admission for the Fintwind relay.
// Upstream Alma relay files (index/shared/apns) stay MIT licensed by yetone;
// this module is Fintwind-specific policy on top. No extra npm dependencies.
//
// Protocol (docs/dev-notes/remote-multiuser.md):
//   POST /v1/admission/challenge {publicKey} -> {nonce, expiresAt}
//   client signs "fintwind.relay.admission.v1\nhttps://fintwind-mobile.roketskiy.top\nNONCE\nPUBLIC_KEY"
//   POST /v1/admission/token {publicKey, nonce, signature} -> {token, expiresAt, installationId}
// Token is HS256 JWT: aud alma-relay, iss fintwind-relay, sub sha256(raw pub), iat, exp = iat + 86400.

import { createHash, createHmac, createPublicKey, randomBytes, timingSafeEqual, verify } from 'node:crypto';

export const ADMISSION_ORIGIN = 'https://fintwind-mobile.roketskiy.top';
export const ADMISSION_PREFIX = 'fintwind.relay.admission.v1';
export const ADMISSION_ISSUER = 'fintwind-relay';
export const ADMISSION_AUDIENCE = 'alma-relay';
export const ADMISSION_TTL_S = 86400;
export const CHALLENGE_TTL_MS = 60_000;
export const CHALLENGE_CAP = 4096;
export const ADMISSION_BODY_MAX = 4096;
export const ADMISSION_TIMEOUT_MS = 10_000;

const PUB_RE = /^[A-Za-z0-9_-]{43}$/;
const NONCE_RE = /^[A-Za-z0-9_-]{43}$/;
const SIG_RE = /^[A-Za-z0-9_-]{86}$/;
const SUB_RE = /^[a-f0-9]{64}$/;

// Ed25519 SPKI DER prefix, so a raw 32-byte key imports without dependencies.
const SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex');

export function isPublicAdmissionEnabled(env) {
    return env.RELAY_PUBLIC_ADMISSION === '1';
}

// High entropy gate: a short or missing secret must never open the relay.
export function tokenSecret(env) {
    const s = String(env.RELAY_TOKEN_SECRET || '');
    if (s.length < 32) return null;
    return s;
}

function b64uEncode(buf) {
    return Buffer.from(buf).toString('base64url');
}

function b64uDecodeStrict(s, expectLen) {
    if (typeof s !== 'string') return null;
    if (!/^[A-Za-z0-9_-]+$/.test(s) || s.includes('=')) return null;
    let out;
    try {
        out = Buffer.from(s, 'base64url');
    } catch {
        return null;
    }
    if (out.length !== expectLen) return null;
    if (b64uEncode(out) !== s) return null;
    return out;
}

function b64uDecodeJson(s, maxLen = 2048) {
    if (typeof s !== 'string' || s.length === 0 || s.length > maxLen) return null;
    if (!/^[A-Za-z0-9_-]+$/.test(s) || s.includes('=')) return null;
    let buf;
    try {
        buf = Buffer.from(s, 'base64url');
    } catch {
        return null;
    }
    if (b64uEncode(buf) !== s) return null;
    try {
        return JSON.parse(buf.toString('utf8'));
    } catch {
        return null;
    }
}

export function installationIdFor(rawPub32) {
    return createHash('sha256').update(rawPub32).digest('hex');
}

export function canonicalMessage(nonceB64u, pubB64u) {
    return `${ADMISSION_PREFIX}\n${ADMISSION_ORIGIN}\n${nonceB64u}\n${pubB64u}`;
}

export function verifyEd25519(messageText, pubB64u, sigB64u) {
    const rawPub = b64uDecodeStrict(pubB64u, 32);
    const rawSig = b64uDecodeStrict(sigB64u, 64);
    if (!rawPub || !rawSig) return false;
    let key;
    try {
        key = createPublicKey({ key: Buffer.concat([SPKI_PREFIX, rawPub]), format: 'der', type: 'spki' });
    } catch {
        return false;
    }
    try {
        return verify(null, Buffer.from(messageText, 'utf8'), key, rawSig);
    } catch {
        return false;
    }
}

export function sweepChallenges(store, nowMs) {
    for (const [nonce, entry] of store) {
        if (!entry || entry.expiresAt <= nowMs) store.delete(nonce);
    }
}

export function createChallenge(store, pubB64u, ip, nowMs = Date.now()) {
    if (!PUB_RE.test(pubB64u || '') || b64uDecodeStrict(pubB64u, 32) === null) {
        return { error: 'Invalid publicKey' };
    }
    if (store.size >= CHALLENGE_CAP) sweepChallenges(store, nowMs);
    if (store.size >= CHALLENGE_CAP) return { error: 'Relay is busy, try again shortly', busy: true };
    const nonce = randomBytes(32).toString('base64url');
    const expiresAt = nowMs + CHALLENGE_TTL_MS;
    store.set(nonce, { pub: pubB64u, ip, expiresAt });
    return { nonce, expiresAt: Math.floor(expiresAt / 1000) };
}

export function consumeChallenge(store, { publicKey, nonce, signature }, ip, nowMs = Date.now()) {
    if (!PUB_RE.test(publicKey || '') || !NONCE_RE.test(nonce || '') || !SIG_RE.test(signature || '')) {
        return { error: 'Invalid challenge response' };
    }
    const entry = store.get(nonce);
    store.delete(nonce);
    if (!entry) return { error: 'Challenge expired or already used' };
    if (entry.expiresAt <= nowMs) return { error: 'Challenge expired or already used' };
    if (entry.pub !== publicKey) return { error: 'Challenge does not match this key' };
    if (entry.ip !== ip) return { error: 'Challenge does not match this network address' };
    const ok = verifyEd25519(canonicalMessage(nonce, publicKey), publicKey, signature);
    if (!ok) return { error: 'Invalid signature' };
    const rawPub = b64uDecodeStrict(publicKey, 32);
    return { installationId: installationIdFor(rawPub) };
}

function signHs256(secret, signingInput) {
    return createHmac('sha256', secret).update(signingInput).digest();
}

export function mintAdmissionToken(secret, installationId, nowSec = Math.floor(Date.now() / 1000)) {
    const header = b64uEncode(Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'JWT' }), 'utf8'));
    const payload = b64uEncode(
        Buffer.from(
            JSON.stringify({ aud: ADMISSION_AUDIENCE, iss: ADMISSION_ISSUER, sub: installationId, iat: nowSec, exp: nowSec + ADMISSION_TTL_S }),
            'utf8'
        )
    );
    const input = `${header}.${payload}`;
    const sig = b64uEncode(signHs256(secret, input));
    return { token: `${input}.${sig}`, expiresAt: nowSec + ADMISSION_TTL_S, installationId };
}

// Strict claims check shared by the HTTP admission path and WebSocket authorize.
export function verifyAdmissionToken(token, secret, nowSec = Math.floor(Date.now() / 1000)) {
    if (!secret || typeof token !== 'string') return null;
    const parts = token.split('.');
    if (parts.length !== 3) return null;
    const header = b64uDecodeJson(parts[0], 512);
    const claims = b64uDecodeJson(parts[1]);
    if (!header || !claims) return null;
    if (header.alg !== 'HS256') return null;
    if (claims.aud !== ADMISSION_AUDIENCE || claims.iss !== ADMISSION_ISSUER) return null;
    if (typeof claims.sub !== 'string' || !SUB_RE.test(claims.sub)) return null;
    if (!Number.isInteger(claims.iat) || !Number.isInteger(claims.exp)) return null;
    if (!(claims.exp > claims.iat)) return null;
    if (!(claims.exp > nowSec)) return null;
    if (claims.iat > nowSec + 60) return null;
    if (claims.exp - claims.iat > ADMISSION_TTL_S + 60) return null;
    const input = `${parts[0]}.${parts[1]}`;
    const sig = b64uDecodeStrict(parts[2], 32);
    if (!sig) return null;
    const want = signHs256(secret, input);
    if (want.length !== sig.length || !timingSafeEqual(want, sig)) return null;
    return claims;
}

// Bounded JSON body reader for admission endpoints: POST only, JSON only,
// 4 KiB max, 10 s timeout. Returns {body} or {status, error}.
export function readAdmissionBody(req, { maxBytes = ADMISSION_BODY_MAX, timeoutMs = ADMISSION_TIMEOUT_MS } = {}) {
    return new Promise(resolve => {
        if (req.method !== 'POST') {
            resolve({ status: 405, error: 'Method not allowed' });
            return;
        }
        const ctype = String(req.headers['content-type'] || '').split(';')[0].trim().toLowerCase();
        if (ctype !== 'application/json') {
            resolve({ status: 415, error: 'Content-Type must be application/json' });
            return;
        }
        const declared = Number(req.headers['content-length'] || 0);
        if (Number.isFinite(declared) && declared > maxBytes) {
            resolve({ status: 413, error: 'Request body too large' });
            req.resume();
            return;
        }
        const chunks = [];
        let size = 0;
        let done = false;
        const finish = value => {
            if (done) return;
            done = true;
            clearTimeout(timer);
            resolve(value);
        };
        const timer = setTimeout(() => {
            req.destroy();
            finish({ status: 408, error: 'Request timed out' });
        }, timeoutMs);
        req.on('data', chunk => {
            if (done) return;
            size += chunk.length;
            if (size > maxBytes) {
                finish({ status: 413, error: 'Request body too large' });
                req.resume();
                return;
            }
            chunks.push(chunk);
        });
        req.on('end', () => {
            if (done) return;
            let body;
            try {
                body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
            } catch {
                finish({ status: 400, error: 'Invalid JSON' });
                return;
            }
            if (!body || typeof body !== 'object' || Array.isArray(body)) {
                finish({ status: 400, error: 'Invalid JSON' });
                return;
            }
            finish({ body });
        });
        req.on('error', () => finish({ status: 400, error: 'Unreadable request' }));
    });
}
