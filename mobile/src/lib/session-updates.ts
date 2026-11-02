import type { SessionDetail } from "./types";
import { isRecord, parseResult, REVISION_RE, textBytes } from "./remote-validation";

const forbidden = new Set(["__proto__", "prototype", "constructor"]);
const invalid = () => new Error("会话增量已失效，请重新读取。");

// Copy only the edited ancestors. Never mutate the displayed or cached snapshot.
function editAt(node: unknown, path: string[], operation: Record<string, unknown>): unknown {
  if (path.length === 0) {
    if (Object.hasOwn(operation, "set")) return operation["set"];
    if (Object.hasOwn(operation, "append")) {
      if (typeof node !== "string" || typeof operation["append"] !== "string") throw invalid();
      const text = node + operation["append"];
      if (textBytes(text) > 65536) throw invalid();
      return text;
    }
    const length = operation["truncate"];
    if (!Array.isArray(node) || !Number.isSafeInteger(length) || Number(length) < 0 || Number(length) > node.length) throw invalid();
    return node.slice(0, Number(length));
  }
  const [key, ...rest] = path;
  if (!key || forbidden.has(key)) throw invalid();
  if (Array.isArray(node)) {
    if (!/^(0|[1-9][0-9]*)$/.test(key)) throw invalid();
    const index = Number(key);
    if (index > node.length || index >= 4096) throw invalid();
    const copy = node.slice();
    copy[index] = editAt(node[index], rest, operation);
    return copy;
  }
  if (!isRecord(node) || !Object.hasOwn(node, key)) throw invalid();
  return { ...node, [key]: editAt(node[key], rest, operation) };
}

export class SessionUpdates {
  private raw: Record<string, unknown> | null = null;
  private detail: SessionDetail | null = null;

  constructor(private readonly sessionId: string) {}

  apply(frame: Record<string, unknown>): SessionDetail {
    let next: unknown;
    if (Object.hasOwn(frame, "snapshot")) {
      if (Object.keys(frame).length !== 3) throw invalid();
      next = frame["snapshot"];
    } else {
      if (Object.keys(frame).length !== 5 || !this.raw || frame["baseRevision"] !== this.raw["revision"] ||
        typeof frame["revision"] !== "string" || !REVISION_RE.test(frame["revision"]) ||
        !Array.isArray(frame["edits"]) || frame["edits"].length > 4096) throw invalid();
      next = this.raw;
      for (const operation of frame["edits"]) {
        if (!isRecord(operation) || Object.keys(operation).length !== 2 ||
          !Array.isArray(operation["path"]) || operation["path"].length > 6 ||
          !operation["path"].every((key) => typeof key === "string" && key.length <= 256 && !forbidden.has(key)) ||
          ["set", "append", "truncate"].filter((key) => Object.hasOwn(operation, key)).length !== 1) throw invalid();
        next = editAt(next, operation["path"], operation);
      }
      if (!isRecord(next) || next["revision"] !== frame["revision"]) throw invalid();
    }
    if (!isRecord(next) || next["id"] !== this.sessionId || typeof next["revision"] !== "string" ||
      !REVISION_RE.test(next["revision"]) || textBytes(JSON.stringify(next)) > 512 * 1024) throw invalid();
    const parsed = parseResult("getSession", next) as SessionDetail;
    // Parsing remains the trust boundary; unchanged row identities survive it.
    for (const key of ["messages", "entries"] as const) {
      const oldRaw = this.raw?.[key], newRaw = next[key];
      const oldRows = this.detail?.[key], newRows = parsed[key];
      if (Array.isArray(oldRaw) && Array.isArray(newRaw) && oldRows && newRows) {
        const previous = new Map(oldRaw.map((value, index) => [value, oldRows[index]]));
        newRows.forEach((value, index) => {
          const old = previous.get(newRaw[index]);
          if (old) newRows[index] = old as typeof value;
        });
      }
    }
    this.raw = next;
    this.detail = parsed;
    return parsed;
  }
}
