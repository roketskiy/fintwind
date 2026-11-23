// Room and connection lifecycle for the signaling service. A room is one
// routeHash (one pairing activation) with at most one desktop and one phone.
// A desktop remains registered while its WebSocket is alive. A token bucket
// limits repeated offers without imposing a lifetime reconnection limit.
// Each attempt permits one offer and one answer; no trickle ICE is supported.

import { Bucket, MAP_CAP } from './limits.mjs';
import { CLOSE } from './protocol.mjs';

export const ROOM_TTL_MS = 60_000;
export const ROOM_IDLE_MS = 30_000;

// A connection that never joins, or a room whose peer never arrives, is cut.
export const JOIN_TIMEOUT_MS = 10_000;

const OPPOSITE = Object.freeze({ desktop: 'phone', phone: 'desktop' });

export function oppositeRole(role) {
    return OPPOSITE[role];
}

// One accepted connection. `joined` gates signaling; before that only a single
// join frame is allowed.
export class Connection {
    constructor(ws, ip) {
        this.ws = ws;
        this.ip = ip;
        this.joined = false;
        this.routeHash = null;
        this.role = null;
        this.isAlive = true;
        this.joinTimer = null;
        this.preJoinFrames = new Bucket(10, 20);
        this.preJoinBytes = new Bucket(64 * 1024, 128 * 1024);
    }
}

export class Room {
    constructor(routeHash, nowMs) {
        this.routeHash = routeHash;
        this.createdAt = nowMs;
        this.expiresAt = nowMs + ROOM_TTL_MS;
        this.lastActivity = nowMs;
        this.attempt = null;
        this.attempts = new Bucket(4 / 60, 4);
        this.sentRoles = new Set();
        this.members = new Map();
    }

    member(role) {
        return this.members.get(role) || null;
    }

    touch(nowMs) {
        this.lastActivity = nowMs;
    }

    expired(nowMs) {
        if (this.member('desktop')) return false;
        return nowMs >= this.expiresAt || nowMs - this.lastActivity >= ROOM_IDLE_MS;
    }

    // Accept one signal for an attempt. A new attempt id starts a fresh
    // generation and resets the per-role set; within one attempt each role may
    // send exactly once. Returns whether the offer/answer exchange is complete.
    acceptSignal(role, attempt, nowMs) {
        if (this.expired(nowMs)) return { ok: false, code: CLOSE.ROOM_LIMIT, message: 'Room expired' };
        if (attempt !== this.attempt) {
            if (role !== 'phone' || !this.attempts.take(1, nowMs)) {
                return { ok: false, code: CLOSE.ROOM_LIMIT, message: 'Attempt rate limited' };
            }
            this.attempt = attempt;
            this.sentRoles = new Set();
        }
        if (this.sentRoles.has(role)) {
            return { ok: false, code: CLOSE.ROOM_LIMIT, message: 'Duplicate signal for attempt' };
        }
        this.sentRoles.add(role);
        this.touch(nowMs);
        return { ok: true, complete: this.sentRoles.size === 2 };
    }

}

// The whole live set. Every map is bounded; over-cap load is denied, not reset.
export class Rooms {
    constructor(maxRooms) {
        this.maxRooms = maxRooms;
        this.byHash = new Map();
    }

    get size() {
        return this.byHash.size;
    }

    // Return the room for this routeHash, creating it if there is capacity.
    // Returns null when the service is full.
    acquire(routeHash, nowMs) {
        let room = this.byHash.get(routeHash);
        if (room) return room;
        if (this.byHash.size >= Math.min(this.maxRooms, MAP_CAP)) return null;
        room = new Room(routeHash, nowMs);
        this.byHash.set(routeHash, room);
        return room;
    }

    remove(routeHash) {
        this.byHash.delete(routeHash);
    }

    removeIfEmpty(room) {
        if (room.members.size === 0 && this.byHash.get(room.routeHash) === room) {
            this.byHash.delete(room.routeHash);
        }
    }

    // Drop expired or empty rooms. Returns the removed rooms so the caller can
    // close their sockets.
    sweep(nowMs) {
        const removed = [];
        for (const [hash, room] of this.byHash) {
            if (room.members.size === 0 || room.expired(nowMs)) {
                this.byHash.delete(hash);
                removed.push(room);
            }
        }
        return removed;
    }
}
