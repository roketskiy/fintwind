import { verifyAdmissionToken } from './admission.mjs';

export const VERSION = '1.2.0-fintwind.1';

// ---------------------------------------------------------------------------
// Agent protocol versions (docs/PROTOCOL.md).
//
//   1  JSON text frames only; request bodies arrive whole, base64, in `hreq`.
//   2  Payload bytes travel in binary frames, and request bodies are streamed
//      to the desktop with a per-request credit window.
//
// The desktop offers `proto` in `register`; the relay answers the version it
// will speak in `registered`. Either side that never mentions it speaks 1, so
// old desktops on new relays and new desktops on old relays keep working.
// ---------------------------------------------------------------------------
export const PROTOCOL = 2;

/** Negotiated version: the lower of what the desktop offers and the relay allows. */
export function negotiateProtocol(offered, relayMax = PROTOCOL) {
    const n = Math.floor(Number(offered));
    const max = Math.max(1, Math.min(PROTOCOL, Math.floor(Number(relayMax)) || PROTOCOL));
    return Number.isFinite(n) && n >= 2 ? Math.min(n, max) : 1;
}

/** Binary frame kinds (protocol 2). */
export const BIN = Object.freeze({
    HCHUNK: 1, // agent -> relay: response body bytes
    HBODY: 2, // relay -> agent: request body bytes
    WBIN: 3, // both: a binary WebSocket message
    WTEXT: 4, // both: a text WebSocket message, UTF-8
});

/** Request body bytes the relay may send ahead of the desktop's `hack`s. */
export const BODY_WINDOW = 4 * 1024 * 1024;
/** Largest request body slice the relay puts in one frame. */
export const BODY_SLICE = 256 * 1024;

const textEncoder = new TextEncoder();

/** `[kind:u8][idLength:u8][id:utf8][payload]` */
export function encodeBinary(kind, id, payload) {
    const idBytes = textEncoder.encode(String(id));
    if (idBytes.length > 255) throw new Error('frame id too long');
    const bytes = typeof payload === 'string' ? textEncoder.encode(payload) : payload;
    const out = new Uint8Array(2 + idBytes.length + bytes.length);
    out[0] = kind;
    out[1] = idBytes.length;
    out.set(idBytes, 2);
    out.set(bytes, 2 + idBytes.length);
    return out;
}

/** -> {kind, id, data} (data is a view into `bytes`), or null if malformed. */
export function decodeBinary(bytes) {
    const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
    if (view.length < 2 || view.length < 2 + view[1]) return null;
    const id = new TextDecoder().decode(view.subarray(2, 2 + view[1]));
    return { kind: view[0], id, data: view.subarray(2 + view[1]) };
}

/** Phone-facing paths look like /c/<code>/rest?query. Never log the code. */
const CLIENT_PATH = /^\/c\/([A-Za-z0-9_-]{16,128})(\/.*)?$/;

/** Parse `/c/<code>/rest?query` -> {code, rest} or null. */
export function parseClientPath(pathWithQuery) {
    const m = CLIENT_PATH.exec(pathWithQuery || '');
    if (!m) return null;
    return { code: m[1], rest: m[2] || '/' };
}

export const isTokenHash = s => typeof s === 'string' && /^[a-f0-9]{64}$/.test(s);

const enc = new TextEncoder();

export async function sha256hex(text) {
    const digest = await crypto.subtle.digest('SHA-256', enc.encode(text));
    return [...new Uint8Array(digest)].map(b => b.toString(16).padStart(2, '0')).join('');
}

export function b64encode(bytes) {
    let binary = '';
    const chunk = 0x8000;
    for (let i = 0; i < bytes.length; i += chunk) {
        binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
    }
    return btoa(binary);
}

export function b64decode(text) {
    const binary = atob(text);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return bytes;
}

/** Constant-time string compare (length leaks, content doesn't). */
export function safeEqual(a, b) {
    const x = enc.encode(String(a));
    const y = enc.encode(String(b));
    if (x.length !== y.length) return false;
    let diff = 0;
    for (let i = 0; i < x.length; i++) diff |= x[i] ^ y[i];
    return diff === 0;
}

/** WebSocket close codes a server may legally send on. */
export function sanitizeCloseCode(code) {
    const n = Number(code);
    if (n === 1000 || (n >= 3000 && n <= 4999)) return n;
    return 1000;
}

export const RELAY_AUDIENCE = 'alma-relay';
export const RELAY_ISSUER = 'fintwind-relay';

/** Returns the token's claims if it is valid, unexpired and for this relay; else null. */
export async function verifyRelayToken(token, secret) {
    return verifyAdmissionToken(token, secret);
}

/**
 * Who may register a desktop (and mint TURN credentials).
 *   RELAY_TOKEN_SECRET set (32+ chars) -> a valid install token is required
 *   LEGACY_REGISTRATION_KEY + future LEGACY_REGISTRATION_UNTIL -> migration key
 *   otherwise -> deny. There is no open relay.
 * `revoked` is an optional Set of banned subject ids; revoked subjects fail.
 * Returns {ok:true, subject, expiresAt} or {ok:false, message, code}.
 */
export async function authorize(env, presented, revoked = null) {
    const secret = String(env.RELAY_TOKEN_SECRET || '');
    if (secret.length < 32) return { ok: false, message: 'Relay is not configured', reason: 'Relay not ready', code: 1013 };
    const claims = await verifyRelayToken(presented, secret);
    if (claims) {
        if (revoked instanceof Set && revoked.has(claims.sub)) {
            return { ok: false, message: 'This install has been revoked', reason: 'Revoked', code: 4403 };
        }
        return { ok: true, subject: String(claims.sub), expiresAt: claims.exp };
    }
    const legacyKey = String(env.LEGACY_REGISTRATION_KEY || '');
    const until = Number(env.LEGACY_REGISTRATION_UNTIL || 0);
    if (legacyKey && Number.isInteger(until) && until > Math.floor(Date.now() / 1000) && safeEqual(presented || '', legacyKey)) {
        return { ok: true, subject: 'legacy-personal', expiresAt: until };
    }
    return { ok: false, message: env.RELAY_TOKEN_MESSAGE || 'A valid relay token is required', reason: 'Relay token required', code: 4401 };
}

// ---------------------------------------------------------------------------
// TURN credentials for the phone<->desktop WebRTC stream (TURN REST API, as
// spoken by coturn's `use-auth-secret`): username "<expiry>:<id>", credential
// = base64(HMAC-SHA1(secret, username)). The relay only mints them; media
// never passes through it. Unset TURN_URLS / TURN_SECRET -> null (404).
// ---------------------------------------------------------------------------
export async function turnCredentials(env, subject) {
    const urls = String(env.TURN_URLS || '')
        .split(',')
        .map(s => s.trim())
        .filter(Boolean);
    if (urls.length === 0 || !env.TURN_SECRET) return null;
    const ttl = Math.min(86400, Math.max(300, Number(env.TURN_TTL) || 43200));
    const expiresAt = Math.floor(Date.now() / 1000) + ttl;
    // The id only feeds coturn's per-user quota; it is never the account itself.
    const id = (await sha256hex(String(subject || 'anon'))).slice(0, 12);
    const username = `${expiresAt}:${id}`;
    const key = await crypto.subtle.importKey('raw', enc.encode(env.TURN_SECRET), { name: 'HMAC', hash: 'SHA-1' }, false, ['sign']);
    const mac = new Uint8Array(await crypto.subtle.sign('HMAC', key, enc.encode(username)));
    return { iceServers: [{ urls, username, credential: b64encode(mac) }], ttl, expiresAt };
}

/** Bearer value of an Authorization header. */
export const bearerOf = header => String(header || '').replace(/^Bearer\s+/i, '').trim();
