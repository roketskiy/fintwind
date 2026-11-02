#!/usr/bin/env node
// Local revocation CLI. Root runs this on the relay host; there is no public
// admin HTTP endpoint. Writes are atomic through a same-directory temp file
// with mode 0600.
import { chmodSync, chownSync, closeSync, openSync, readFileSync, renameSync, statSync, unlinkSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { randomBytes } from 'node:crypto';
import { REVOKED_CAP, REVOKED_MAX_BYTES, isValidSubjectId, revokedFile } from './revocation.mjs';

function usage() {
    console.log('usage: node admin.mjs list|revoke <subject-id>|allow <subject-id>');
    console.log('env: RELAY_REVOKED_FILE (default /etc/fintwind-alma-relay/revoked.json)');
}

function readList(file) {
    let raw;
    try {
        raw = readFileSync(file);
    } catch (err) {
        if (err && err.code === 'ENOENT') return [];
        throw err;
    }
    if (raw.length > REVOKED_MAX_BYTES) throw new Error('Revocation file too large');
    const parsed = JSON.parse(raw.toString('utf8'));
    if (!Array.isArray(parsed)) throw new Error('Revocation file must be a JSON array');
    return parsed;
}

function writeListAtomic(file, list) {
    if (list.length > REVOKED_CAP) throw new Error('Revocation list exceeds cap');
    for (const id of list) {
        if (!isValidSubjectId(id)) throw new Error(`Invalid subject id: ${id}`);
    }
    const dir = dirname(resolve(file));
    const tmp = join(dir, `.${basename(file)}.${process.pid}.${randomBytes(4).toString('hex')}.tmp`);
    let created = false;
    try {
        writeFileSync(tmp, JSON.stringify([...new Set(list)].sort(), null, 2) + '\n', { mode: 0o600, flag: 'wx' });
        created = true;
        chownSync(tmp, 0, statSync(dir).gid);
        chmodSync(tmp, 0o640);
        renameSync(tmp, file);
    } catch (err) {
        if (created) { try { unlinkSync(tmp); } catch {} }
        throw err;
    }
}

function withWriteLock(file, operation) {
    if (typeof process.getuid !== 'function' || process.getuid() !== 0) throw new Error('Run this command as root on the relay server');
    const lock = `${file}.lock`;
    const descriptor = openSync(lock, 'wx', 0o600);
    try {
        return operation();
    } finally {
        closeSync(descriptor);
        unlinkSync(lock);
    }
}

const [cmd, arg] = process.argv.slice(2);
const file = revokedFile(process.env);
if (cmd === 'list') {
    try {
        for (const id of readList(file)) console.log(id);
    } catch (err) {
        console.error(`[admin] ${err instanceof Error ? err.message : err}`);
        process.exit(1);
    }
} else if (cmd === 'revoke' && arg) {
    if (!isValidSubjectId(arg)) {
        console.error('[admin] subject id must be 64 lowercase hex chars');
        process.exit(1);
    }
    try {
        withWriteLock(file, () => {
            const list = readList(file);
            if (!list.includes(arg)) list.push(arg);
            writeListAtomic(file, list);
        });
        console.log(`[admin] revoked ${arg}`);
    } catch (err) {
        console.error(`[admin] ${err instanceof Error ? err.message : err}`);
        process.exit(1);
    }
} else if (cmd === 'allow' && arg) {
    if (!isValidSubjectId(arg)) {
        console.error('[admin] subject id must be 64 lowercase hex chars');
        process.exit(1);
    }
    try {
        withWriteLock(file, () => writeListAtomic(file, readList(file).filter(id => id !== arg)));
        console.log(`[admin] allowed ${arg}`);
    } catch (err) {
        console.error(`[admin] ${err instanceof Error ? err.message : err}`);
        process.exit(1);
    }
} else {
    usage();
    process.exit(2);
}
