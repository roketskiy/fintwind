// Bounded resource helpers for the signaling service. Every table has a cap
// and an expiry sweep; over-cap load is denied instead of resetting live
// counters. Self-contained so the signal service deploys without the relay.

import { isIP } from 'node:net';

export const MAP_CAP = 4096;
export const PENDING_PER_IP = 2;
export const PENDING_GLOBAL = 64;
export const UPGRADE_PER_MIN = 30;
export const UPGRADE_BURST = 20;
export const WINDOW_MS = 60_000;

function sweepExpired(map, nowMs) {
    for (const [key, entry] of map) {
        if (entry && entry.expiresAt <= nowMs) map.delete(key);
    }
}

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
