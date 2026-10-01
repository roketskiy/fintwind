/**
 * Credential hand-off for the explicitly authorized real-provider E2E.
 *
 * Failure modes: copying the user's session DB; selecting another account;
 * printing a key in an error/report; forwarding all provider credentials; or
 * persisting a key in config. Only the active StepFun row is read, read-only.
 * This module does not start a server, change an account, or send a prompt.
 */
import { Database } from 'bun:sqlite';

export const STEPFUN_PROVIDER = 'stepfun-ai-step-plan';
export const STEPFUN_MODEL = 'step-5-preview';

export function selectedStepFunKey(userDb: string): string {
  let db: Database | undefined;
  try {
    db = new Database(userDb, { readonly: true });
    db.exec('PRAGMA query_only = 1');
    const row = db.query(`SELECT value FROM credential
      WHERE integration_id = ? AND active = 1
      ORDER BY time_updated DESC LIMIT 1`).get(STEPFUN_PROVIDER) as { value?: unknown } | null;
    if (!row) throw new Error('missing');
    const raw = typeof row.value === 'string' ? row.value
      : row.value instanceof Uint8Array ? new TextDecoder().decode(row.value) : undefined;
    if (!raw || raw.length > 16 * 1024) throw new Error('invalid');
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== 'object' || !('key' in value)
      || typeof value.key !== 'string' || !value.key.trim() || value.key.length > 8 * 1024) {
      throw new Error('invalid');
    }
    return value.key;
  } catch {
    // Do not attach the SQL row, JSON parse error, or key to diagnostics.
    throw new Error('The active StepFun credential could not be read safely. Supply STEPFUN_API_KEY or a valid read-only credential database.');
  } finally {
    db?.close();
  }
}

/** Give the credential to one owned child, never process.env or a disk file. */
export function stepFunChildEnv(base: NodeJS.ProcessEnv, userDb?: string): NodeJS.ProcessEnv {
  const inherited = base.STEPFUN_API_KEY;
  const key = inherited?.trim() ? inherited : userDb ? selectedStepFunKey(userDb) : undefined;
  if (!key) throw new Error('Real-provider E2E requires an authorized StepFun credential.');
  const env = { ...base };
  // An isolated E2E should not activate a second vendor through an ambient
  // credential. StepFun may expose several catalogs; the model must still be
  // explicitly selected using STEPFUN_PROVIDER and STEPFUN_MODEL above.
  for (const name of Object.keys(env)) {
    if (/(?:API_KEY|APIKEY|ACCESS_TOKEN|AUTH_TOKEN|SECRET_KEY)$/i.test(name)) delete env[name];
  }
  env.STEPFUN_API_KEY = key;
  return env;
}
