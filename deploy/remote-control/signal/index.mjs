#!/usr/bin/env node
// Fintwind direct-connect signaling service (protocol v2).
//
//   desktop --outbound WSS-->  signal  <--WSS--  phone
//
// Both sides connect to /signal/v2 and join a room keyed by the SHA-256 of the
// pairing route. The service matches the two roles and forwards only opaque,
// pairing-key-protected signaling ciphertext (SDP and ICE). It never sees the
// pairing key, never parses business data, and offers no relay, TURN, HTTP
// forwarding or legacy channel. WebRTC business data flows peer to peer.
//
// Run behind a TLS-terminating proxy (Caddy) in production.

import { createServer } from 'node:http';
import { WebSocketServer, WebSocket } from 'ws';
import { WindowLimiter, Bucket, PendingTracker, trustedIp, MAP_CAP, UPGRADE_PER_MIN, UPGRADE_BURST } from './limits.mjs';
import {
    VERSION,
    SIGNAL_PATH,
    MAX_MESSAGE_BYTES,
    CLOSE,
    parseJoin,
    parseSignal,
    joinedFrame,
    peerFrame,
    forwardSignalFrame,
    errorFrame,
} from './protocol.mjs';
import { Rooms, Connection, oppositeRole, JOIN_TIMEOUT_MS } from './room.mjs';

const env = process.env;
const PORT = Number(env.PORT || 8790);
const HOST = env.HOST || '127.0.0.1';

const limit = (value, fallback) => {
    const n = Number(value);
    return value !== undefined && value !== '' && Number.isFinite(n) && n >= 0 ? n : fallback;
};
const MAX_ROOMS = limit(env.MAX_ROOMS, 64) || 64;
const MAX_CONNECTIONS = limit(env.MAX_CONNECTIONS, 256) || 256;
const MAX_JOINED_CONNECTIONS = Math.min(MAX_ROOMS * 2, MAX_CONNECTIONS);
const PING_INTERVAL_MS = 30_000;
const SWEEP_INTERVAL_MS = 5_000;
const SHUTDOWN_CLOSE_MS = 1_000;
// A recipient that cannot drain this many buffered bytes is cut, not buffered.
const HIGH_WATER = 1024 * 1024;

const rooms = new Rooms(MAX_ROOMS);
const connections = new Set();

const pending = new PendingTracker();
const upgradeLimits = new WindowLimiter(UPGRADE_PER_MIN, UPGRADE_PER_MIN * 8);
const upgradeBursts = new Map();

function upgradeAllowed(ip, nowMs = Date.now()) {
    let bucket = upgradeBursts.get(ip);
    if (!bucket) {
        if (upgradeBursts.size >= MAP_CAP) return false;
        bucket = new Bucket(UPGRADE_PER_MIN / 60, UPGRADE_BURST);
        upgradeBursts.set(ip, bucket);
    }
    return bucket.take(1, nowMs) && upgradeLimits.take(ip, nowMs);
}

function sendJson(ws, frame) {
    if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(frame));
}

function closeWith(ws, code, message, retryAfterMs) {
    try {
        sendJson(ws, errorFrame(message, retryAfterMs));
    } catch {}
    try {
        ws.close(code, message);
    } catch {}
}

// Tell the other role whether `role` is present. Presence is idempotent.
function announcePresence(room, role, online) {
    const other = room.member(oppositeRole(role));
    if (other && other.joined) sendJson(other.ws, peerFrame(role, online));
}

// Detach a connection from its room. `notify` controls whether the peer is told
// the role went offline; a same-role replacement keeps the role online.
function detach(conn, notify) {
    if (!conn.joined) return;
    const routeHash = conn.routeHash;
    const role = conn.role;
    const room = rooms.byHash.get(routeHash);
    conn.joined = false;
    conn.routeHash = null;
    conn.role = null;
    if (!room) return;
    if (room.member(role) === conn) {
        room.members.delete(role);
        if (role === 'phone') room.clearPhoneAttempt();
        if (notify) announcePresence(room, role, false);
        rooms.removeIfEmpty(room);
    }
}

function handleConnection(ws, ip) {
    const conn = new Connection(ws, ip);
    connections.add(conn);

    conn.joinTimer = setTimeout(() => {
        if (!conn.joined) closeWith(ws, CLOSE.JOIN_TIMEOUT, 'Join timed out');
    }, JOIN_TIMEOUT_MS);

    ws.on('message', (data, isBinary) => {
        if (ws.readyState !== WebSocket.OPEN) return;
        if (isBinary) {
            closeWith(ws, CLOSE.MALFORMED, 'Binary frames are not allowed');
            return;
        }
        if (data.length > MAX_MESSAGE_BYTES) {
            closeWith(ws, CLOSE.TOO_BIG, 'Message too large');
            return;
        }
        if (!conn.joined) {
            if (!conn.preJoinFrames.take(1) || !conn.preJoinBytes.take(data.length)) {
                closeWith(ws, CLOSE.RATE_LIMIT, 'Too many frames before join');
                return;
            }
        }
        let frame;
        try {
            frame = JSON.parse(data.toString());
        } catch {
            closeWith(ws, CLOSE.MALFORMED, 'Invalid JSON');
            return;
        }
        if (!conn.joined) {
            handleJoin(conn, frame);
            return;
        }
        handleSignal(conn, frame);
    });

    ws.on('pong', () => {
        conn.isAlive = true;
    });

    ws.on('close', () => {
        clearTimeout(conn.joinTimer);
        conn.joinTimer = null;
        detach(conn, true);
        connections.delete(conn);
    });

    ws.on('error', () => {});
}

function handleJoin(conn, frame) {
    const parsed = parseJoin(frame);
    if (!parsed.ok) {
        closeWith(conn.ws, parsed.code, parsed.message);
        return;
    }
    const nowMs = Date.now();
    if (connections.size > MAX_JOINED_CONNECTIONS) {
        closeWith(conn.ws, CLOSE.SERVER_BUSY, 'Service is full');
        return;
    }
    const room = rooms.acquire(parsed.routeHash, nowMs);
    if (!room) {
        closeWith(conn.ws, CLOSE.SERVER_BUSY, 'Service is full');
        return;
    }
    if (room.expired(nowMs)) {
        for (const member of room.members.values()) {
            detach(member, false);
            connections.delete(member);
            closeWith(member.ws, CLOSE.ROOM_LIMIT, 'Room expired');
        }
        rooms.removeIfEmpty(room);
        closeWith(conn.ws, CLOSE.ROOM_LIMIT, 'Room expired');
        return;
    }

    // One connection per role. A newer connection replaces the older one so a
    // phone or desktop can reconnect after a network change without a new room.
    const existing = room.member(parsed.role);
    if (existing && existing !== conn) {
        detach(existing, false);
        connections.delete(existing);
        closeWith(existing.ws, CLOSE.REPLACED, 'Replaced by a newer connection');
    }

    clearTimeout(conn.joinTimer);
    conn.joinTimer = null;
    conn.joined = true;
    conn.routeHash = parsed.routeHash;
    conn.role = parsed.role;
    room.members.set(parsed.role, conn);
    room.touch(nowMs);

    sendJson(conn.ws, joinedFrame());
    // Tell the newcomer about the peer, and the peer about the newcomer.
    const other = room.member(oppositeRole(parsed.role));
    if (other && other.joined) sendJson(conn.ws, peerFrame(other.role, true));
    announcePresence(room, parsed.role, true);
}

function handleSignal(conn, frame) {
    const parsed = parseSignal(frame);
    if (!parsed.ok) {
        closeWith(conn.ws, parsed.code, parsed.message);
        return;
    }
    const room = rooms.byHash.get(conn.routeHash);
    if (!room || room.member(conn.role) !== conn) {
        closeWith(conn.ws, CLOSE.NOT_JOINED, 'Not in a room');
        return;
    }
    const nowMs = Date.now();
    const accepted = room.acceptSignal(conn.role, parsed.attempt, nowMs);
    if (!accepted.ok) {
        if (accepted.code === CLOSE.ROOM_LIMIT && room.expired(nowMs)) {
            for (const member of room.members.values()) {
                detach(member, false);
                connections.delete(member);
                closeWith(member.ws, CLOSE.ROOM_LIMIT, 'Room expired');
            }
            rooms.removeIfEmpty(room);
        }
        closeWith(conn.ws, accepted.code, accepted.message, accepted.retryAfterMs);
        return;
    }
    if (accepted.ignored) return;

    const peer = room.member(oppositeRole(conn.role));
    if (peer && peer.joined && peer.ws.readyState === WebSocket.OPEN) {
        const outbound = forwardSignalFrame(conn.role, parsed);
        const outboundBytes = Buffer.byteLength(JSON.stringify(outbound));
        if (peer.ws.bufferedAmount + outboundBytes > HIGH_WATER) {
            closeWith(peer.ws, CLOSE.RATE_LIMIT, 'Recipient is not draining');
        } else {
            peer.ws.send(JSON.stringify(outbound));
        }
    }

}

const server = createServer((req, res) => {
    const pathname = (req.url || '').split('?')[0];
    if (pathname === '/healthz') {
        res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
        res.end(JSON.stringify({ ok: true, service: 'fintwind-signal', version: VERSION, rooms: rooms.size }));
        return;
    }
    res.writeHead(404, { 'content-type': 'application/json', 'cache-control': 'no-store' });
    res.end(JSON.stringify({ error: 'Not found' }));
    req.resume();
});

const wss = new WebSocketServer({ noServer: true, maxPayload: MAX_MESSAGE_BYTES });
if (MAX_CONNECTIONS) server.maxConnections = MAX_CONNECTIONS;

function refuseUpgrade(socket, status) {
    socket.write(`HTTP/1.1 ${status}\r\nconnection: close\r\ncontent-length: 0\r\n\r\n`);
    socket.destroy();
}

server.on('upgrade', (req, socket, head) => {
    const url = (req.url || '').split('?')[0];
    if (url !== SIGNAL_PATH) return refuseUpgrade(socket, '404 Not Found');
    const ip = trustedIp(req, socket);
    if (!upgradeAllowed(ip)) return refuseUpgrade(socket, '429 Too Many Requests');
    if (!pending.acquire(ip)) return refuseUpgrade(socket, '503 Service Unavailable');
    let released = false;
    const release = () => {
        if (!released) {
            released = true;
            pending.release(ip);
        }
    };
    socket.on('close', release);
    wss.handleUpgrade(req, socket, head, ws => {
        release();
        handleConnection(ws, ip);
    });
});

const sweeper = setInterval(() => {
    const nowMs = Date.now();
    for (const room of rooms.sweep(nowMs)) {
        for (const conn of room.members.values()) {
            detach(conn, false);
            connections.delete(conn);
            closeWith(conn.ws, CLOSE.ROOM_LIMIT, 'Room closed');
        }
    }
    upgradeLimits.sweep(nowMs);
    for (const [ip, bucket] of upgradeBursts) {
        if (bucket.last && nowMs - bucket.last > 600_000) upgradeBursts.delete(ip);
    }
}, SWEEP_INTERVAL_MS);
sweeper.unref?.();

const pinger = setInterval(() => {
    for (const conn of connections) {
        if (!conn.isAlive) {
            try {
                conn.ws.terminate();
            } catch {}
            continue;
        }
        conn.isAlive = false;
        try {
            conn.ws.ping();
        } catch {}
    }
}, PING_INTERVAL_MS);
pinger.unref?.();

server.listen(PORT, HOST, () => {
    console.log(`[signal] Fintwind signaling v${VERSION} listening on ${HOST}:${PORT} (path ${SIGNAL_PATH})`);
});

for (const signal of ['SIGTERM', 'SIGINT']) {
    process.on(signal, () => {
        clearInterval(sweeper);
        clearInterval(pinger);
        for (const conn of connections) {
            try {
                conn.ws.close(1012, 'Service restarting');
            } catch {}
        }
        server.close();
        server.closeAllConnections();
        setTimeout(() => process.exit(0), SHUTDOWN_CLOSE_MS).unref();
    });
}
