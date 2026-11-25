// Strict wire contract for the Fintwind direct-connect signaling service (v2).
// The server only ever sees an opaque, pairing-key-protected ciphertext. It
// validates the outer envelope and forwards it to the other role in the same
// room. It never parses or inspects the ciphertext.
//
// Only a complete offer/answer exchange is supported, with no trickle ICE. Each
// attempt allows exactly one signal per role, both at seq 0: the phone's offer
// and the desktop's answer. The server cannot tell business data from signaling
// inside the ciphertext, so it bounds the exchange by count, size and time
// rather than by content.

export const VERSION = 2;
export const SIGNAL_PATH = '/signal/v2';

// One WebSocket frame may not exceed 64 KiB. A complete (non-trickle) SDP
// offer/answer bundles all ICE candidates and must fit within this bound.
export const MAX_MESSAGE_BYTES = 64 * 1024;

export const ROLES = Object.freeze(['desktop', 'phone']);

const ROUTE_HASH_RE = /^[a-f0-9]{64}$/;
const B64URL_RE = /^[A-Za-z0-9_-]+$/;
// 32 bytes, unpadded base64url, is exactly 43 characters.
const ATTEMPT_RE = /^[A-Za-z0-9_-]{43}$/;

// Application close codes. The server sends only legal WebSocket codes.
export const CLOSE = Object.freeze({
    MALFORMED: 4400,
    NOT_JOINED: 4401,
    JOIN_TIMEOUT: 4408,
    ROOM_LIMIT: 4410,
    ROOM_COMPLETE: 4411,
    RATE_LIMIT: 4429,
    REPLACED: 4000,
    SERVER_BUSY: 1013,
    TOO_BIG: 1009,
});

export function isRouteHash(value) {
    return typeof value === 'string' && ROUTE_HASH_RE.test(value);
}

export function isRole(value) {
    return value === 'desktop' || value === 'phone';
}

// Strict unpadded base64url decode. Returns the bytes, or null when the text is
// not canonical base64url or does not decode to exactly `expectBytes` (when set).
export function decodeB64Url(text, expectBytes = null) {
    if (typeof text !== 'string' || text.length === 0 || !B64URL_RE.test(text) || text.includes('=')) return null;
    let bytes;
    try {
        bytes = Buffer.from(text, 'base64url');
    } catch {
        return null;
    }
    if (bytes.length === 0) return null;
    if (expectBytes !== null && bytes.length !== expectBytes) return null;
    if (Buffer.from(bytes).toString('base64url') !== text) return null;
    return bytes;
}

function isPlainObject(value) {
    return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function hasOnlyKeys(value, allowed) {
    return Object.keys(value).every(key => allowed.includes(key));
}

// Parse the first frame a connection must send. Returns a discriminated result.
export function parseJoin(frame) {
    if (!isPlainObject(frame)) return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid join' };
    if (!hasOnlyKeys(frame, ['type', 'version', 'routeHash', 'role'])) {
        return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid join fields' };
    }
    if (frame.type !== 'join' || frame.version !== VERSION) return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid join' };
    if (!isRouteHash(frame.routeHash)) return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid routeHash' };
    if (!isRole(frame.role)) return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid role' };
    return { ok: true, routeHash: frame.routeHash, role: frame.role };
}

// Parse a signaling envelope. `data` stays opaque; only its shape and size are
// checked. Returns the parsed envelope or a failure.
export function parseSignal(frame) {
    if (!isPlainObject(frame)) return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid signal' };
    if (!hasOnlyKeys(frame, ['type', 'version', 'attempt', 'seq', 'data'])) {
        return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid signal fields' };
    }
    if (frame.type !== 'signal' || frame.version !== VERSION) return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid signal' };
    if (!ATTEMPT_RE.test(frame.attempt || '') || decodeB64Url(frame.attempt, 32) === null) {
        return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid attempt' };
    }
    if (!Number.isSafeInteger(frame.seq) || frame.seq < 0) return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid seq' };
    if (frame.seq !== 0) return { ok: false, code: CLOSE.MALFORMED, message: 'seq must be 0' };
    if (typeof frame.data !== 'string' || frame.data.length === 0 || frame.data.length > MAX_MESSAGE_BYTES) {
        return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid data' };
    }
    if (decodeB64Url(frame.data) === null) return { ok: false, code: CLOSE.MALFORMED, message: 'Invalid data' };
    return { ok: true, attempt: frame.attempt, seq: frame.seq, data: frame.data };
}

export function joinedFrame() {
    return { type: 'joined', version: VERSION };
}

export function peerFrame(role, online) {
    return { type: 'peer', version: VERSION, role, online };
}

// A forwarded signal carries the sender role so the receiver can bind it.
export function forwardSignalFrame(sender, envelope) {
    return { type: 'signal', version: VERSION, sender, attempt: envelope.attempt, seq: envelope.seq, data: envelope.data };
}

export function errorFrame(message, retryAfterMs) {
    const frame = { type: 'error', version: VERSION, message };
    if (retryAfterMs !== undefined) frame.retryAfterMs = retryAfterMs;
    return frame;
}
