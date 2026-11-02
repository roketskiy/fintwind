// Bounded quota tables for the relay. Every map has a cap and an expiry
// sweep; over-cap load is denied instead of resetting live counters.

import { isIP } from 'node:net';

export const MAP_CAP = 4096;
export const PENDING_PER_IP = 2;
export const PENDING_GLOBAL = 64;
export const UPGRADE_PER_MIN = 30;
export const UPGRADE_BURST = 10;
export const WINDOW_MS = 60_000;

function sweepExpired(map, nowMs) {
    for (const [key, entry] of map) {
        if (entry && entry.expiresAt <= nowMs) map.delete(key);
    }
}

// Sliding window: per-key limit plus a global limit in one table.
export class WindowLimiter {
    constructor(perKey, global, windowMs = WINDOW_MS) {
        this.perKey = perKey;
        this.global = global;
        this.windowMs = windowMs;
        this.keys = new Map();
        this.globalCount = 0;
        this.globalStart = 0;
    }

    take(key, nowMs = Date.now()) {
        if (this.keys.size >= MAP_CAP) sweepExpired(this.keys, nowMs);
        if (nowMs - this.globalStart >= this.windowMs) {
            this.globalStart = nowMs;
            this.globalCount = 0;
        }
        let slot = this.keys.get(key);
        if (!slot || nowMs - slot.start >= this.windowMs) {
            slot = { count: 0, start: nowMs, expiresAt: nowMs + this.windowMs };
            if (this.keys.size >= MAP_CAP && !this.keys.has(key)) return false;
            this.keys.set(key, slot);
        }
        if (slot.count >= this.perKey || this.globalCount >= this.global) return false;
        slot.count++;
        this.globalCount++;
        return true;
    }

    sweep(nowMs = Date.now()) {
        sweepExpired(this.keys, nowMs);
    }
}

// Token bucket for frame and byte rates.
export class Bucket {
    constructor(ratePerSec, burst) {
        this.ratePerSec = ratePerSec;
        this.burst = burst;
        this.tokens = burst;
        this.last = 0;
    }

    take(n, nowMs = Date.now()) {
        if (!this.last) this.last = nowMs;
        const elapsed = Math.max(0, nowMs - this.last) / 1000;
        this.tokens = Math.min(this.burst, this.tokens + elapsed * this.ratePerSec);
        this.last = nowMs;
        if (this.tokens < n) return false;
        this.tokens -= n;
        return true;
    }
}

// Per-subject pipes in each direction: 60 frames/s burst 120,
// 512 KiB/s burst 1 MiB. Deny means fail closed upstream.
export class SubjectPipes {
    constructor() {
        this.subjects = new Map();
    }

    pipe(subject) {
        let pipe = this.subjects.get(subject);
        if (!pipe) {
            if (this.subjects.size >= MAP_CAP) return null;
            pipe = {
                upFrames: new Bucket(60, 120),
                upBytes: new Bucket(512 * 1024, 1024 * 1024),
                downFrames: new Bucket(60, 120),
                downBytes: new Bucket(512 * 1024, 1024 * 1024),
                inactiveAt: 0,
            };
            this.subjects.set(subject, pipe);
        }
        return pipe;
    }

    consume(subject, dir, bytes, nowMs = Date.now()) {
        const pipe = this.pipe(subject);
        if (!pipe) return false;
        pipe.inactiveAt = 0;
        const frames = dir === 'up' ? pipe.upFrames : pipe.downFrames;
        const flow = dir === 'up' ? pipe.upBytes : pipe.downBytes;
        if (!frames.take(1, nowMs)) return false;
        if (!flow.take(bytes, nowMs)) return false;
        return true;
    }

    drop(subject) {
        const pipe = this.subjects.get(subject);
        if (pipe) pipe.inactiveAt = Date.now();
    }

    sweep(nowMs = Date.now()) {
        for (const [subject, pipe] of this.subjects) {
            if (pipe.inactiveAt && nowMs - pipe.inactiveAt >= 120_000) this.subjects.delete(subject);
        }
    }
}

// Upgrade permits: at most 2 pending per IP and 64 globally.
export class PendingTracker {
    constructor(perIp = PENDING_PER_IP, global = PENDING_GLOBAL) {
        this.perIp = perIp;
        this.global = global;
        this.counts = new Map();
        this.total = 0;
    }

    acquire(ip) {
        const n = this.counts.get(ip) || 0;
        if (n >= this.perIp || this.total >= this.global) return false;
        this.counts.set(ip, n + 1);
        this.total++;
        return true;
    }

    release(ip) {
        const n = this.counts.get(ip) || 0;
        if (n <= 0) return;
        if (n === 1) this.counts.delete(ip);
        else this.counts.set(ip, n - 1);
        this.total = Math.max(0, this.total - 1);
    }
}

export function normalizeIp(addr) {
    if (typeof addr !== 'string' || !addr) return null;
    let ip = addr.trim();
    if (ip.startsWith('::ffff:')) ip = ip.slice('::ffff:'.length);
    if (!isIP(ip)) return null;
    return ip;
}

function isLoopbackIp(ip) {
    return ip === '127.0.0.1' || ip === '::1';
}

// Trust X-Forwarded-For only from the loopback Caddy hop, and only as one
// validated single IP. Any chain or invalid value falls back to the socket.
export function trustedIp(req, socket) {
    const direct = normalizeIp(socket?.remoteAddress || '');
    const raw = req?.headers?.['x-forwarded-for'];
    if (direct && isLoopbackIp(direct) && typeof raw === 'string' && raw) {
        const value = raw.trim();
        if (!value.includes(',')) {
            const forwarded = normalizeIp(value);
            if (forwarded && !isLoopbackIp(forwarded)) return forwarded;
        }
        return direct;
    }
    return direct || 'unknown';
}
