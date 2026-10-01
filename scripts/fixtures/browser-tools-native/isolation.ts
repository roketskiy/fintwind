/**
 * Failure modes: an SDK persists an environment credential in a DB/WAL; a
 * chunk boundary hides that credential; cleanup follows a junction into user
 * data; or a generic recursive delete reaches beyond the one owned run.
 * Scan all bytes, including databases, and remove only the verified UUID root.
 */
import { createReadStream } from 'node:fs';
import { lstat, opendir, realpath, rm } from 'node:fs/promises';
import { basename, join, resolve } from 'node:path';

export async function inspectAndRemoveIsolation(directory: string, runId: string, secrets: string[]) {
  if (!/^[0-9a-f-]{36}$/i.test(runId) || basename(directory) !== `browser-tools-native-${runId}`) {
    throw new Error('Refusing cleanup of a directory not owned by this exact run.');
  }
  const root = resolve(directory);
  const metadata = await lstat(root);
  if (!metadata.isDirectory() || metadata.isSymbolicLink() || resolve(await realpath(root)) !== root) {
    throw new Error('Refusing cleanup of a redirected isolation root.');
  }
  const needles = secrets.filter(Boolean).map(secret => Buffer.from(secret));
  const overlap = Math.max(0, ...needles.map(needle => needle.length - 1));
  const deadline = Date.now() + 30_000;
  let files = 0;
  let bytes = 0;
  let matchedFiles = 0;
  try {
    const scan = async (path: string): Promise<void> => {
      if (Date.now() >= deadline || files > 10_000 || bytes > 1024 * 1024 * 1024) {
        throw new Error('Isolation credential scan exceeded its budget.');
      }
      const info = await lstat(path);
      if (info.isSymbolicLink()) throw new Error('Isolation credential scan refused a redirected path.');
      if (info.isDirectory()) {
        const entries = await opendir(path);
        for await (const entry of entries) await scan(join(path, entry.name));
        return;
      }
      if (!info.isFile()) throw new Error('Isolation credential scan encountered a non-file object.');
      files += 1;
      let tail = Buffer.alloc(0);
      let matched = false;
      const stream = createReadStream(path, { highWaterMark: 64 * 1024 });
      for await (const chunk of stream) {
        if (Date.now() >= deadline || bytes > 1024 * 1024 * 1024) {
          throw new Error('Isolation credential scan exceeded its budget.');
        }
        const value = Buffer.concat([tail, chunk as Buffer]);
        bytes += (chunk as Buffer).length;
        if (needles.some(needle => value.includes(needle))) matched = true;
        tail = overlap ? value.subarray(Math.max(0, value.length - overlap)) : Buffer.alloc(0);
      }
      if (matched) matchedFiles += 1;
    };
    await scan(root);
  } finally {
    // Only this run's root, never its parent or the user credential database.
    // JSON reports live outside the isolation root and remain reproducible.
    await rm(root, { recursive: true, force: false, maxRetries: 3, retryDelay: 200 });
  }
  return { files, bytes, matchedFiles, removed: true };
}
