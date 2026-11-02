// Root-owned revocation list for installed identities.
// File holds a JSON array of subject ids (sha256 hex of the install pubkey).
// Reads are bounded and fail closed; the service never writes this file.

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

export const REVOKED_DEFAULT_FILE = '/etc/fintwind-alma-relay/revoked.json';
export const REVOKED_MAX_BYTES = 1024 * 1024;
export const REVOKED_CAP = 4096;
export const REVOKED_REFRESH_MS = 5000;
export const SUB_RE = /^[a-f0-9]{64}$/;

export function revokedFile(env) {
    const p = String(env.RELAY_REVOKED_FILE || '').trim();
    return p || REVOKED_DEFAULT_FILE;
}

export function isValidSubjectId(s) {
    return typeof s === 'string' && SUB_RE.test(s);
}

// {ok, revoked:Set} on success; {ok:false, error} when missing or malformed.
export function loadRevokedFile(file) {
    let raw;
    try {
        raw = readFileSync(file);
    } catch (err) {
        return { ok: false, error: err instanceof Error ? err.message : String(err) };
    }
    if (raw.length > REVOKED_MAX_BYTES) return { ok: false, error: 'Revocation file too large' };
    let parsed;
    try {
        parsed = JSON.parse(raw.toString('utf8'));
    } catch {
        return { ok: false, error: 'Revocation file is not valid JSON' };
    }
    if (!Array.isArray(parsed)) return { ok: false, error: 'Revocation file must be a JSON array' };
    if (parsed.length > REVOKED_CAP) return { ok: false, error: 'Revocation list exceeds cap' };
    const revoked = new Set();
    for (const id of parsed) {
        if (!isValidSubjectId(id)) return { ok: false, error: 'Revocation list holds an invalid subject id' };
        revoked.add(id);
    }
    return { ok: true, revoked };
}

export function revokedDir(file) {
    return dirname(resolve(file));
}
