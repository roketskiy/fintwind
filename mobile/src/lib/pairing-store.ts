import { decodeBase64Url, validToken } from "./remote-crypto";

export interface SavedPairing { route: string; material: CryptoKey }
const DATABASE = "fintwind-phone";
const STORE = "pairing";
let opening: Promise<IDBDatabase> | undefined;

function database(): Promise<IDBDatabase> {
  return opening ??= new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1);
    let expired = false;
    const fail = (message: string) => { expired = true; window.clearTimeout(timeout); opening = undefined; reject(new Error(message)); };
    const timeout = window.setTimeout(() => fail("无法打开配对存储，请允许此网站保存数据。"), 8000);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onerror = () => fail("无法读取配对存储。");
    request.onblocked = () => fail("请关闭其他远控页面后重试。");
    request.onsuccess = () => {
      window.clearTimeout(timeout);
      const db = request.result;
      if (expired) { db.close(); return; }
      db.onversionchange = () => { db.close(); opening = undefined; };
      resolve(db);
    };
  });
}

async function transaction<T>(mode: IDBTransactionMode, run: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const db = await database();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE, mode);
    const request = run(tx.objectStore(STORE));
    const timeout = window.setTimeout(() => { try { tx.abort(); } catch { /* Already completed. */ } }, 8000);
    tx.oncomplete = () => { window.clearTimeout(timeout); resolve(request.result); };
    tx.onabort = tx.onerror = () => { window.clearTimeout(timeout); reject(new Error("无法保存配对，请允许此网站保存数据。")); };
  });
}

export async function loadPairing(): Promise<SavedPairing | null> {
  const value: unknown = await transaction("readonly", (store) => store.get("current"));
  if (value == null) return null;
  const pair = value as Partial<SavedPairing>;
  if (!validToken(pair.route) || !(pair.material instanceof CryptoKey) || pair.material.extractable
    || pair.material.algorithm.name !== "HKDF" || !pair.material.usages.includes("deriveKey")) {
    throw new Error("保存的配对无效，请重新扫码。" );
  }
  return pair as SavedPairing;
}

export async function rememberPairing(route: string, key: string): Promise<SavedPairing> {
  if (!validToken(route) || !validToken(key)) throw new Error("配对链接无效");
  const bytes = decodeBase64Url(key);
  const material = await crypto.subtle.importKey("raw", new Uint8Array(bytes), "HKDF", false, ["deriveKey"]);
  bytes.fill(0);
  const pair = { route, material };
  await transaction("readwrite", (store) => store.put(pair, "current"));
  return pair;
}

export async function forgetPairing(): Promise<void> {
  await transaction("readwrite", (store) => store.delete("current"));
  sessionStorage.removeItem("fintwind-pair");
}
