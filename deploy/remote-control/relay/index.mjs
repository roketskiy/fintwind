#!/usr/bin/env node
// Alma Relay — Node edition.
//
// A self-hostable reverse tunnel that lets the Alma mobile app reach a
// desktop Alma from anywhere:
//
//   desktop (agent)  --outbound WSS-->  relay  <--HTTPS/WSS--  phone (client)
//
// The desktop dials OUT to this server and registers with the SHA-256 hash of
// a routing token derived from its connect code. Phones talk plain
// HTTP/WebSocket to `https://relay.host/c/<token>/...`; the relay hashes the
// token from the URL, finds the matching agent, and tunnels the request over
// the agent socket. Nothing is written to disk; only hashes are kept, in memory.
//
// The wire protocol is documented in docs/PROTOCOL.md. Configuration is all
// environment variables — see README.md or .env.example.
//
// Run behind a TLS-terminating proxy (Caddy / nginx / Cloudflare) in production.

import { createServer } from 'node:http';
import { connect as http2Connect, constants as http2Constants } from 'node:http2';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { WebSocketServer, WebSocket } from 'ws';
import { apnsSignerFromEnv, apnsTopic, apnsHost, apnsRequestHeaders, clampBatch, validateNotification, resultFromResponse } from './apns.mjs';
import { createChallenge, consumeChallenge, isPublicAdmissionEnabled, mintAdmissionToken, readAdmissionBody, sweepChallenges, tokenSecret } from './admission.mjs';
import { loadRevokedFile, REVOKED_REFRESH_MS, revokedFile } from './revocation.mjs';
import { PendingTracker, SubjectPipes, WindowLimiter, Bucket, trustedIp, MAP_CAP, PENDING_GLOBAL, PENDING_PER_IP, UPGRADE_BURST, UPGRADE_PER_MIN } from './quotas.mjs';
import {
    BIN,
    VERSION,
    authorize,
    bearerOf,
    decodeBinary,
    encodeBinary,
    isTokenHash,
    negotiateProtocol,
    parseClientPath,
    sanitizeCloseCode,
    turnCredentials,
} from './shared.mjs';

const env = process.env;
const PORT = Number(env.PORT || 8787);
const HOST = env.HOST || '0.0.0.0';
const MB = 1024 * 1024;
/** A non-negative number from the environment, or the default. 0 means "no limit". */
const limit = (value, fallback) => {
    const n = Number(value);
    return value !== undefined && value !== '' && Number.isFinite(n) && n >= 0 ? n : fallback;
};
const MAX_AGENTS = limit(env.MAX_AGENTS, 32) || 32; // install desktops, fail closed at cap
const MAX_AGENTS_PER_SUBJECT = limit(env.MAX_AGENTS_PER_SUBJECT, 1) || 1;
const MAX_AGENTS_PER_IP = limit(env.MAX_AGENTS_PER_IP, 8) || 8;
const MAX_SOCKETS_PER_AGENT = 1;
const MAX_CONNECTIONS = limit(env.MAX_CONNECTIONS, 256) || 256; // TCP connections to this process
const STALL_MS = limit(env.STALL_TIMEOUT_S, 60) * 1000; // a phone that drains nothing for this long is cut off
// Highest agent protocol this relay speaks (docs/PROTOCOL.md). 1 turns off
// binary frames and request body streaming for every desktop.
const RELAY_PROTOCOL = negotiateProtocol(env.RELAY_PROTOCOL || 2);
const REGISTER_TIMEOUT_MS = 10_000; // an agent socket must register within this
const PING_INTERVAL_MS = 30_000;
// Frame caps: 1 MiB each way, so one desktop cannot hog the relay.
const AGENT_MAX_FRAME = 1 * MB;
const CLIENT_MAX_MESSAGE = 1 * MB; // phone and agent symmetric
const OPEN_QUEUE_MAX = 128 * 1024; // phone bytes held while the desktop opens its side
// Flow control: above HIGH_WATER buffered bytes the sender is paused until
// the buffer drains below LOW_WATER.
const HIGH_WATER = 1 * MB;
const LOW_WATER = 256 * 1024;
const FLOW_TICK_MS = 25;

// ---------------------------------------------------------------------------
// APNs (optional). With no key the relay answers `unsupported` and the
// desktop stops asking. The key is read once at startup and never logged.
// ---------------------------------------------------------------------------
const apnsEnv = {
    ...env,
    APNS_KEY: env.APNS_KEY || (env.APNS_KEY_FILE ? readFileSync(env.APNS_KEY_FILE, 'utf8') : ''),
};
let apnsSigner = null;
try {
    apnsSigner = apnsSignerFromEnv(apnsEnv);
} catch (err) {
    console.error('[relay] APNs key misconfigured:', err instanceof Error ? err.message : err);
}
const APNS_TOPIC = apnsTopic(apnsEnv);

/** One notification over a fresh HTTP/2 session (Node's fetch is HTTP/1.1 only). */
function apnsPostHttp2(host, path, headers, body) {
    return new Promise((resolve, reject) => {
        const session = http2Connect(`https://${host}`);
        const finish = fn => value => {
            session.close();
            fn(value);
        };
        session.on('error', finish(reject));
        const req = session.request({ ...headers, [http2Constants.HTTP2_HEADER_METHOD]: 'POST', [http2Constants.HTTP2_HEADER_PATH]: path });
        let status = 0;
        const chunks = [];
        req.on('response', h => {
            status = Number(h[http2Constants.HTTP2_HEADER_STATUS] || 0);
        });
        req.on('data', c => chunks.push(c));
        req.on(
            'end',
            finish(() => resolve({ status, body: Buffer.concat(chunks).toString('utf8') }))
        );
        req.on('error', finish(reject));
        req.setTimeout(15_000, () => req.close(http2Constants.NGHTTP2_CANCEL));
        req.end(body);
    });
}

async function sendPushBatch(notifications) {
    const results = [];
    for (const n of clampBatch(notifications)) {
        const invalid = validateNotification(n);
        if (invalid) {
            results.push({ token: typeof n?.token === 'string' ? n.token : '', status: 400, reason: invalid });
            continue;
        }
        try {
            const auth = await apnsSigner.token();
            const { status, body } = await apnsPostHttp2(
                apnsHost(n.environment),
                `/3/device/${n.token}`,
                apnsRequestHeaders(auth, APNS_TOPIC, n),
                JSON.stringify(n.payload)
            );
            results.push({ token: n.token, ...resultFromResponse(status, body) });
        } catch (err) {
            results.push({ token: n.token, status: 0, reason: err instanceof Error ? err.message : String(err) });
        }
    }
    return results;
}

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

/** tokenHash -> Agent */
const agents = new Map();
/** subject -> tokenHash for the one-desktop-per-install rule */
const subjectIndex = new Map();

// Persistent-install admission state. All tables stay bounded; overload is
// denied instead of resetting live counters.
const challenges = new Map();
const pending = new PendingTracker(PENDING_PER_IP, PENDING_GLOBAL);
const upgradeLimits = new WindowLimiter(UPGRADE_PER_MIN, UPGRADE_PER_MIN * 8);
const upgradeBursts = new Map(); // ip -> Bucket(30/min, burst 10), bounded
const challengeLimits = new WindowLimiter(10, 120);
const tokenLimits = new WindowLimiter(10, 120);
const pipes = new SubjectPipes();

function upgradeAllowed(ip, nowMs = Date.now()) {
    let bucket = upgradeBursts.get(ip);
    if (!bucket) {
        if (upgradeBursts.size >= MAP_CAP) return false;
        bucket = new Bucket(UPGRADE_PER_MIN / 60, UPGRADE_BURST);
        upgradeBursts.set(ip, bucket);
    }
    return bucket.take(1, nowMs) && upgradeLimits.take(ip, nowMs);
}

let revokedSet = new Set();
let revokedOk = false;

function refreshRevocations() {
    sweepChallenges(challenges, Date.now());
    for (const [ip, bucket] of upgradeBursts) {
        if (bucket.last && Date.now() - bucket.last > 600_000) upgradeBursts.delete(ip);
    }
    upgradeLimits.sweep();
    challengeLimits.sweep();
    tokenLimits.sweep();
    pipes.sweep();
    const loaded = loadRevokedFile(revokedFile(env));
    revokedOk = loaded.ok;
    if (loaded.ok) {
        revokedSet = loaded.revoked;
        for (const agent of [...agents.values()]) {
            if (agent.owner?.subject && revokedSet.has(agent.owner.subject)) {
                try {
                    agent.destroy('Install revoked');
                    agent.ws.terminate();
                } catch {}
            }
        }
    } else {
        for (const agent of agents.values()) {
            agent.destroy('Relay policy unavailable');
            agent.ws.terminate();
        }
    }
}
refreshRevocations();
const revokedTimer = setInterval(refreshRevocations, REVOKED_REFRESH_MS);
revokedTimer.unref?.();

const sha256 = s => createHash('sha256').update(s).digest('hex');

class Agent {
    constructor(ws, tokenHash, name, proto = 1, owner = null) {
        this.ws = ws;
        this.tokenHash = tokenHash;
        this.name = String(name || 'desktop').slice(0, 64);
        /** Negotiated protocol version (see shared.mjs) */
        this.proto = proto;
        /** {subject, ip, expiresAt} for quota ownership and token renewal */
        this.owner = owner;
        this.expiryTimer = null;
        this.nextId = 1;
        /** id -> {res, headSent, timer} for in-flight HTTP requests */
        this.http = new Map();
        /** id -> {client, opened, queue} for bridged WebSockets */
        this.sockets = new Map();
        /** HTTP requests from phones that are still uploading or waiting */
        this.inflight = 0;
        this.isAlive = true;
        // Flow control. Downstream (desktop -> phone): when a phone can't keep
        // up, stop reading the agent socket, so TCP pushes back on the desktop
        // (which waits for its socket to drain). Upstream (phone -> desktop):
        // when the agent socket's send buffer is full, stop reading the phones.
        /** key -> {level, kill, last, since}: phone sinks over HIGH_WATER */
        this.sinks = new Map();
        /** phone WebSockets and uploads paused until the agent socket drains */
        this.waiting = new Set();
        this.paused = false;
        this.flowTimer = null;
    }

    send(frame) {
        if (this.ws.readyState === WebSocket.OPEN) {
            this.ws.send(JSON.stringify(frame));
        }
    }

    /** A binary frame (protocol 2 only). */
    sendBinary(kind, id, payload) {
        if (this.ws.readyState === WebSocket.OPEN) {
            this.ws.send(encodeBinary(kind, id, payload));
        }
    }

    /** A phone WebSocket message, in the form this desktop speaks. */
    sendWs(id, data, isBinary) {
        if (this.proto >= 2) this.sendBinary(isBinary ? BIN.WBIN : BIN.WTEXT, id, data);
        else this.send(isBinary ? { t: 'wmsg', id, b: Buffer.from(data).toString('base64') } : { t: 'wmsg', id, s: data.toString() });
    }

    id(prefix) {
        return `${prefix}${this.nextId++}`;
    }

    /**
     * A phone sink has more than HIGH_WATER bytes queued. `level()` returns
     * its buffered bytes, or -1 once it is gone; `kill()` cuts it off when it
     * drains nothing for STALL_MS.
     */
    congested(key, level, kill) {
        if (!this.sinks.has(key)) this.sinks.set(key, { level, kill, last: level(), since: Date.now() });
        if (!this.paused) {
            this.paused = true;
            this.ws.pause();
        }
        this.startFlow();
    }

    /** Pause a phone socket or upload while the agent socket is backed up. */
    throttle(source) {
        if (this.ws.bufferedAmount <= HIGH_WATER || this.waiting.has(source)) return;
        source.pause();
        this.waiting.add(source);
        this.startFlow();
    }

    startFlow() {
        if (!this.flowTimer) this.flowTimer = setInterval(() => this.flowTick(), FLOW_TICK_MS);
    }

    flowTick() {
        const now = Date.now();
        for (const [key, sink] of this.sinks) {
            const level = sink.level();
            if (level < LOW_WATER) {
                this.sinks.delete(key);
            } else if (level < sink.last) {
                sink.last = level;
                sink.since = now;
            } else if (STALL_MS && now - sink.since > STALL_MS) {
                this.sinks.delete(key);
                try {
                    sink.kill();
                } catch {}
            }
        }
        if (this.paused && !this.sinks.size) {
            this.paused = false;
            this.isAlive = true; // pongs were not read while paused
            if (this.ws.readyState === WebSocket.OPEN) this.ws.resume();
        }
        if (this.waiting.size && (this.ws.bufferedAmount < LOW_WATER || this.ws.readyState !== WebSocket.OPEN)) {
            for (const source of this.waiting) source.resume();
            this.waiting.clear();
        }
        if (!this.sinks.size && !this.waiting.size) this.stopFlow();
    }

    stopFlow() {
        clearInterval(this.flowTimer);
        this.flowTimer = null;
    }

    /** Fail in-flight requests and close every bridged phone socket. */
    destroy(reason = 'Desktop disconnected') {
        this.stopFlow();
        if (this.expiryTimer) {
            clearTimeout(this.expiryTimer);
            this.expiryTimer = null;
        }
        this.sinks.clear();
        this.waiting.clear();
        for (const [id, entry] of this.http) {
            clearTimeout(entry.timer);
            this.http.delete(id);
        }
        for (const [id, bridge] of this.sockets) {
            try {
                bridge.client.close(1012, reason);
            } catch {}
            this.sockets.delete(id);
        }
    }
}

function agentCountForSubject(subject) {
    let n = 0;
    for (const agent of agents.values()) {
        if (agent.owner?.subject === subject) n++;
    }
    return n;
}

function agentCountForIp(ip) {
    let n = 0;
    for (const agent of agents.values()) {
        if (agent.owner?.ip === ip) n++;
    }
    return n;
}

// Token expiry ends the agent socket so the client renews. Firing the timer
// only closes this exact socket; replacement sockets are unaffected.
function armTokenExpiry(agent) {
    const ms = (agent.owner?.expiresAt || 0) * 1000 - Date.now();
    if (!(ms > 0)) return;
    agent.expiryTimer = setTimeout(() => {
        if (agents.get(agent.tokenHash) === agent) {
            agents.delete(agent.tokenHash);
            if (agent.owner?.subject && subjectIndex.get(agent.owner.subject) === agent.tokenHash) {
                subjectIndex.delete(agent.owner.subject);
            }
            if (agent.owner?.subject && agentCountForSubject(agent.owner.subject) === 0) {
                pipes.drop(agent.owner.subject);
            }
            agent.destroy('Relay token expired');
            try {
                agent.ws.close(4401, 'Relay token expired');
            } catch {}
        }
    }, Math.min(ms, 2_147_483_647));
    agent.expiryTimer.unref?.();
}

// Per-subject frame and byte budgets in each direction. Over budget the relay
// fails closed: it closes the agent socket instead of silently dropping data.
function withinBudget(agent, dir, bytes) {
    const subject = agent.owner?.subject || 'unknown';
    if (pipes.consume(subject, dir, bytes)) return true;
    try {
        agent.ws.close(1013, 'Rate limit exceeded');
    } catch {}
    return false;
}

function respondJson(res, status, obj, extraHeaders = {}) {
    if (res.destroyed) return;
    if (res.headersSent) {
        res.destroy();
        return;
    }
    res.writeHead(status, { 'content-type': 'application/json', 'access-control-allow-origin': '*', ...extraHeaders });
    res.end(JSON.stringify(obj));
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

const server = createServer((req, res) => {
    const url = req.url || '';
    const pathname = url.split('?')[0];

    if (pathname === '/healthz') {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ ok: true, edition: 'node', version: VERSION, protocol: RELAY_PROTOCOL, agents: agents.size }));
        return;
    }

    if (pathname === '/') {
        res.writeHead(200, { 'content-type': 'text/plain; charset=utf-8' });
        res.end(`Alma relay ${VERSION} is running.\nUse this address in Alma → Settings → Remote Access → Use a self-hosted relay.\n`);
        return;
    }

    if (pathname === '/turn') {
        if (req.method !== 'POST' && req.method !== 'GET') {
            respondJson(res, 405, { error: 'Method not allowed' });
            return;
        }
        (async () => {
            const auth = await authorize(env, bearerOf(req.headers.authorization), revokedOk ? revokedSet : null);
            if (!auth.ok) return respondJson(res, 401, { error: auth.message });
            if (!revokedOk) return respondJson(res, 503, { error: 'Relay is not ready, try again shortly' });
            const creds = await turnCredentials(env, auth.subject || bearerOf(req.headers.authorization));
            if (!creds) return respondJson(res, 404, { error: 'TURN not configured on this relay' });
            respondJson(res, 200, creds);
        })().catch(() => respondJson(res, 500, { error: 'TURN credentials failed' }));
        return;
    }

    if (pathname === '/v1/admission/challenge' || pathname === '/v1/admission/token') {
        const ip = trustedIp(req, req.socket);
        if (!isPublicAdmissionEnabled(env)) {
            respondJson(res, 403, { error: 'Public admission is disabled' });
            req.resume();
            return;
        }
        const limiter = pathname === '/v1/admission/challenge' ? challengeLimits : tokenLimits;
        if (!limiter.take(ip)) {
            respondJson(res, 429, { error: 'Too many requests, try again shortly' }, { 'retry-after': '60', connection: 'close' });
            return;
        }
        (async () => {
            const secret = tokenSecret(env);
            if (!secret) {
                respondJson(res, 503, { error: 'Relay is not configured for admission' });
                return;
            }
            if (!revokedOk) {
                respondJson(res, 503, { error: 'Relay is not ready, try again shortly' });
                return;
            }
            const read = await readAdmissionBody(req);
            if (read.error) {
                respondJson(res, read.status, { error: read.error });
                return;
            }
            if (pathname === '/v1/admission/challenge') {
                const made = createChallenge(challenges, read.body.publicKey, ip);
                if (made.error) {
                    respondJson(res, made.busy ? 503 : 400, { error: made.error });
                    return;
                }
                respondJson(res, 200, { nonce: made.nonce, expiresAt: made.expiresAt });
                return;
            }
            const { publicKey, nonce, signature } = read.body;
            const consumed = consumeChallenge(challenges, { publicKey, nonce, signature }, ip);
            if (consumed.error) {
                respondJson(res, 400, { error: consumed.error });
                return;
            }
            if (revokedSet.has(consumed.installationId)) {
                respondJson(res, 403, { error: 'This install has been revoked' });
                return;
            }
            const minted = mintAdmissionToken(secret, consumed.installationId);
            respondJson(res, 200, minted);
        })().catch(() => respondJson(res, 500, { error: 'Admission failed' }));
        return;
    }

    const parsed = parseClientPath(url);
    if (!parsed) {
        respondJson(res, 404, { error: 'Not found' });
        return;
    }

    // Phone HTTP forwarding is disabled: the relay tunnels one fixed
    // WebSocket path per desktop and nothing else.
    respondJson(res, 403, { error: 'HTTP forwarding is disabled on this relay' });
    req.resume();
    return;
});

// ---------------------------------------------------------------------------
// WebSockets — one server in noServer mode for /agent and phone bridges
// ---------------------------------------------------------------------------

const agentWss = new WebSocketServer({ noServer: true, maxPayload: AGENT_MAX_FRAME });
const clientWss = new WebSocketServer({ noServer: true, maxPayload: CLIENT_MAX_MESSAGE });
if (MAX_CONNECTIONS) server.maxConnections = MAX_CONNECTIONS;

function refuseUpgrade(socket, status) {
    socket.write(`HTTP/1.1 ${status}\r\nconnection: close\r\ncontent-length: 0\r\n\r\n`);
    socket.destroy();
}

server.on('upgrade', (req, socket, head) => {
    const url = req.url || '';
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
    if (url === '/agent' || url.startsWith('/agent?')) {
        agentWss.handleUpgrade(req, socket, head, ws => handleAgent(ws, ip, release));
        return;
    }
    const parsed = parseClientPath(url);
    if (parsed && parsed.rest === '/__fintwind_remote__') {
        const agent = agents.get(sha256(parsed.code));
        if (!agent) {
            release();
            return refuseUpgrade(socket, '502 Bad Gateway');
        }
        if (MAX_SOCKETS_PER_AGENT && agent.sockets.size >= MAX_SOCKETS_PER_AGENT) {
            release();
            return refuseUpgrade(socket, '503 Service Unavailable');
        }
        clientWss.handleUpgrade(req, socket, head, ws => {
            release();
            handleClientSocket(ws, agent, parsed.rest);
        });
        return;
    }
    release();
    refuseUpgrade(socket, '404 Not Found');
});

function handleAgent(ws, ip, releasePermit) {
    let agent = null;
    let registering = false;
    const unauthenticatedFrames = new Bucket(10, 20);
    const unauthenticatedBytes = new Bucket(64 * 1024, 128 * 1024);
    // Sockets that never register would otherwise sit here forever.
    const registerTimer = setTimeout(() => {
        if (!agent) ws.terminate();
    }, REGISTER_TIMEOUT_MS);

    const deny = (message, code, reason) => {
        try {
            ws.send(JSON.stringify({ t: 'error', message }));
        } catch {}
        try {
            ws.close(code, reason);
        } catch {}
        releasePermit();
    };

    ws.on('message', (data, isBinary) => {
        processAgentMessage(data, isBinary).catch(() => ws.terminate());
    });

    async function processAgentMessage(data, isBinary) {
        if (ws.readyState !== WebSocket.OPEN) return;
        if (agent) {
            if (agents.get(agent.tokenHash) !== agent || !withinBudget(agent, 'down', data.length)) {
                ws.terminate();
                return;
            }
        } else if (!unauthenticatedFrames.take(1) || !unauthenticatedBytes.take(data.length)) {
            ws.terminate();
            return;
        }
        if (isBinary && agent && agent.proto >= 2) {
            onBinaryFrame(agent, data);
            return;
        }
        let frame;
        try {
            frame = JSON.parse(data.toString());
        } catch {
            return;
        }
        if (!frame || typeof frame !== 'object' || Array.isArray(frame)) {
            ws.terminate();
            return;
        }

        if (frame.t === 'register') {
            if (agent || registering) return;
            if (typeof frame.key !== 'string' || frame.key.length > 4096
                || (frame.name !== undefined && (typeof frame.name !== 'string' || frame.name.length > 128))
                || (frame.proto !== undefined && !Number.isSafeInteger(frame.proto))) {
                deny('Invalid registration', 4002, 'Invalid registration');
                return;
            }
            registering = true;
            const auth = await authorize(env, frame.key, revokedOk ? revokedSet : null);
            if (!auth.ok) {
                deny(auth.message, auth.code, auth.reason);
                return;
            }
            if (!revokedOk) {
                deny('Relay is not ready, try again shortly', 1013, 'Relay not ready');
                return;
            }
            if (revokedOk && revokedSet.has(auth.subject)) {
                deny('This install has been revoked', 4403, 'Revoked');
                return;
            }
            if (!isTokenHash(frame.tokenHash)) {
                deny('Invalid token hash', 4002, 'Invalid token hash');
                return;
            }
            if (ws.readyState !== WebSocket.OPEN) {
                releasePermit();
                return;
            }
            // Atomic section: no await between these checks and agents.set,
            // so reentrant register frames cannot lose a quota permit.
            const owner = { subject: auth.subject, ip, expiresAt: auth.expiresAt || 0 };
            const existing = agents.get(frame.tokenHash);
            if (existing && existing.owner?.subject !== owner.subject) {
                deny('This route is already registered', 4409, 'Route taken');
                return;
            }
            if (existing && existing.owner?.ip !== ip && agentCountForIp(ip) >= MAX_AGENTS_PER_IP) {
                deny('Too many desktops from this address', 1013, 'Too many desktops from this address');
                return;
            }
            if (!existing) {
                if (agents.size >= MAX_AGENTS) {
                    deny('This relay is full', 1013, 'Relay is full');
                    return;
                }
                const held = subjectIndex.get(owner.subject);
                if (held && held !== frame.tokenHash) {
                    deny('This install already has a desktop', 4409, 'Install already registered');
                    return;
                }
                if (agentCountForSubject(owner.subject) >= MAX_AGENTS_PER_SUBJECT) {
                    deny('This install already has a desktop', 4409, 'Install already registered');
                    return;
                }
                if (agentCountForIp(ip) >= MAX_AGENTS_PER_IP) {
                    deny('Too many desktops from this address', 1013, 'Too many desktops from this address');
                    return;
                }
            }
            clearTimeout(registerTimer);
            if (existing) {
                existing.destroy('Desktop reconnected');
                try {
                    existing.ws.close(4000, 'Replaced by a newer connection');
                } catch {}
            }
            const proto = negotiateProtocol(frame.proto, RELAY_PROTOCOL);
            agent = new Agent(ws, frame.tokenHash, frame.name, proto, owner);
            agents.set(frame.tokenHash, agent);
            if (owner.subject) subjectIndex.set(owner.subject, frame.tokenHash);
            armTokenExpiry(agent);
            releasePermit();
            ws.send(JSON.stringify({ t: 'registered', proto }));
            console.log(`[relay] agent registered for ${owner.subject}; agents=${agents.size}`);
            return;
        }

        if (!agent) return;

        switch (frame.t) {
            case 'push': {
                const id = String(frame.id || '');
                if (!apnsSigner) {
                    agent.send({ t: 'pushed', id, unsupported: true });
                    return;
                }
                sendPushBatch(frame.notifications).then(results => {
                    const failed = results.filter(r => r.status !== 200);
                    if (failed.length)
                        console.warn(
                            `[relay] push: ${failed.length}/${results.length} rejected`,
                            failed.map(r => r.reason || r.status)
                        );
                    agent.send({ t: 'pushed', id, results });
                });
                return;
            }
            // HTTP tunneling is disabled on this relay, so there is never
            // a pending HTTP entry. Answer hcancel and drop the bytes.
            case 'hres':
            case 'hchunk':
            case 'hack':
            case 'hend':
            case 'herr': {
                agent.send({ t: 'hcancel', id: frame.id });
                break;
            }
            case 'wopened': {
                const bridge = agent.sockets.get(frame.id);
                if (!bridge) {
                    agent.send({ t: 'wclose', id: frame.id, code: 1000 });
                    return;
                }
                bridge.opened = true;
                for (const queued of bridge.queue) {
                    agent.sendWs(frame.id, queued.data, queued.isBinary);
                }
                bridge.queue = [];
                bridge.queued = 0;
                agent.throttle(bridge.client);
                break;
            }
            case 'werr': {
                const bridge = agent.sockets.get(frame.id);
                if (!bridge) return;
                agent.sockets.delete(frame.id);
                try {
                    bridge.client.close(1011, 'Desktop could not open the socket');
                } catch {}
                break;
            }
            case 'wmsg':
                if (typeof frame.s === 'string') sendToPhone(agent, frame.id, frame.s, false);
                else if (typeof frame.b === 'string') sendToPhone(agent, frame.id, Buffer.from(frame.b, 'base64'), true);
                break;
            case 'wclose': {
                const bridge = agent.sockets.get(frame.id);
                if (!bridge) return;
                agent.sockets.delete(frame.id);
                try {
                    bridge.client.close(sanitizeCloseCode(frame.code), String(frame.reason || '').slice(0, 120));
                } catch {}
                break;
            }
        }
    }

    ws.on('pong', () => {
        if (agent) agent.isAlive = true;
    });

    ws.on('close', () => {
        clearTimeout(registerTimer);
        releasePermit();
        if (agent) agent.stopFlow();
        if (agent && agents.get(agent.tokenHash) === agent) {
            agents.delete(agent.tokenHash);
            if (agent.owner?.subject && subjectIndex.get(agent.owner.subject) === agent.tokenHash) {
                subjectIndex.delete(agent.owner.subject);
            }
            if (agent.owner?.subject && agentCountForSubject(agent.owner.subject) === 0) {
                pipes.drop(agent.owner.subject);
            }
            agent.destroy();
            console.log(`[relay] agent disconnected; agents=${agents.size}`);
        }
    });

    ws.on('error', () => {});
}

function onBinaryFrame(agent, data) {
    const frame = decodeBinary(data);
    if (!frame) return;
    switch (frame.kind) {
        case BIN.WBIN:
            return sendToPhone(agent, frame.id, frame.data, true);
        case BIN.WTEXT:
            return sendToPhone(agent, frame.id, frame.data, false);
        default:
            return;
    }
}

/** `data` is a string or bytes; a text message may arrive as UTF-8 bytes. */
function sendToPhone(agent, id, data, isBinary) {
    const bridge = agent.sockets.get(id);
    if (!bridge || bridge.client.readyState !== WebSocket.OPEN) return;
    const { client } = bridge;
    client.send(data, { binary: isBinary });
    if (client.bufferedAmount > HIGH_WATER) {
        agent.congested(
            `w:${id}`,
            () => (client.readyState === WebSocket.OPEN ? client.bufferedAmount : -1),
            () => client.terminate() // its close handler tells the desktop
        );
    }
}

function handleClientSocket(clientWs, agent, path) {
    const id = agent.id('w');
    const bridge = { client: clientWs, opened: false, queue: [], queued: 0 };
    agent.sockets.set(id, bridge);
    agent.send({ t: 'wopen', id, path });

    clientWs.on('message', (data, isBinary) => {
        const bytes = data.length;
        if (!withinBudget(agent, 'up', bytes)) {
            try {
                clientWs.close(1013, 'Rate limit exceeded');
            } catch {}
            return;
        }
        if (bridge.opened) {
            agent.sendWs(id, data, isBinary);
            agent.throttle(clientWs);
            return;
        }
        bridge.queued += data.length;
        if (bridge.queued > OPEN_QUEUE_MAX) {
            bridge.queue = [];
            clientWs.close(1009, 'Too much data before the desktop answered');
            return;
        }
        bridge.queue.push({ data, isBinary });
    });

    clientWs.on('close', (code, reason) => {
        if (agent.sockets.has(id)) {
            agent.sockets.delete(id);
            agent.send({ t: 'wclose', id, code: sanitizeCloseCode(code), reason: reason?.toString() });
        }
    });

    clientWs.on('error', () => {});
}

// Terminate dead agent connections.
const pinger = setInterval(() => {
    for (const agent of agents.values()) {
        if (agent.paused) continue; // pongs aren't read while paused; flow control has its own timeout
        if (!agent.isAlive) {
            try {
                agent.ws.terminate();
            } catch {}
            continue;
        }
        agent.isAlive = false;
        try {
            agent.ws.ping();
        } catch {}
    }
}, PING_INTERVAL_MS);

server.listen(PORT, HOST, () => {
    const secret = tokenSecret(env);
    const admission = isPublicAdmissionEnabled(env) ? 'public-admission' : 'admission-closed';
    const gate = secret ? `install tokens (${admission})` : 'locked (no valid RELAY_TOKEN_SECRET)';
    if (!secret) console.error('[relay] RELAY_TOKEN_SECRET missing or too short: agent registration is denied');
    if (!revokedOk) console.error('[relay] revocation list unreadable at startup: admission is denied until it loads');
    const extras = [apnsSigner && 'APNs', env.TURN_URLS && env.TURN_SECRET && 'TURN'].filter(Boolean);
    console.log(`[relay] Alma relay ${VERSION} listening on ${HOST}:${PORT} (access: ${gate}${extras.length ? `; ${extras.join(', ')}` : ''})`);
});

// Docker sends SIGTERM; close cleanly so desktops reconnect right away.
for (const signal of ['SIGTERM', 'SIGINT']) {
    process.on(signal, () => {
        clearInterval(pinger);
        clearInterval(revokedTimer);
        for (const agent of agents.values()) {
            agent.destroy('Relay shutting down');
            try {
                agent.ws.close(1012, 'Relay restarting');
            } catch {}
        }
        server.close(() => process.exit(0));
        setTimeout(() => process.exit(0), 3000).unref();
    });
}
