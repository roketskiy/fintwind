const BASE64URL_RE = /^[A-Za-z0-9_-]+$/;
export const MAX_SAFE_SEQ = Number.MAX_SAFE_INTEGER;
export const FRAME_MAX_BYTES = 1024 * 1024;
export const RESPONSE_MAX_BYTES = 512 * 1024;
const HASH_RE = /^[a-f0-9]{64}$/;
const SIGNAL_MAX_BYTES = 48 * 1024;

export function encodeBase64Url(bytes: Uint8Array): string {
  let text = "";
  for (const byte of bytes) text += String.fromCharCode(byte);
  return btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function decodeBase64Url(text: string): Uint8Array<ArrayBuffer> {
  if (!BASE64URL_RE.test(text)) throw new Error("配对信息无效");
  const base = text.replace(/-/g, "+").replace(/_/g, "/");
  return Uint8Array.from(
    atob(base + "=".repeat((4 - (base.length % 4)) % 4)),
    (c) => c.charCodeAt(0),
  );
}

export function validToken(text: unknown): text is string {
  if (typeof text !== "string") return false;
  try {
    const bytes = decodeBase64Url(text);
    return bytes.length === 32 && encodeBase64Url(bytes) === text;
  } catch {
    return false;
  }
}

export async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(value),
  );
  return Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

export function validHash(value: unknown): value is string {
  return typeof value === "string" && HASH_RE.test(value);
}

export async function deriveSignalKeys(
  secret: CryptoKey,
  attempt: Uint8Array,
): Promise<[CryptoKey, CryptoKey]> {
  const derive = (role: "phone" | "desktop"): Promise<CryptoKey> =>
    crypto.subtle.deriveKey(
      {
        name: "HKDF",
        hash: "SHA-256",
        salt: new Uint8Array(attempt),
        info: new TextEncoder().encode(`fintwind.direct.v2.signal.${role}`),
      },
      secret,
      { name: "AES-GCM", length: 256 },
      false,
      role === "phone" ? ["encrypt"] : ["decrypt"],
    );
  return Promise.all([derive("phone"), derive("desktop")]);
}

export async function sealSignal(
  key: CryptoKey,
  sequence: number,
  aad: string,
  value: { kind: "offer" | "answer"; sdp: string },
): Promise<string> {
  if (!Number.isSafeInteger(sequence) || sequence < 0 || sequence > MAX_SAFE_SEQ)
    throw new Error("信令序号无效");
  const encrypted = await crypto.subtle.encrypt(
    {
      name: "AES-GCM",
      iv: makeNonce(sequence),
      additionalData: new TextEncoder().encode(aad),
      tagLength: 128,
    },
    key,
    new TextEncoder().encode(JSON.stringify(value)),
  );
  if (encrypted.byteLength > SIGNAL_MAX_BYTES) throw new Error("信令数据过大");
  return encodeBase64Url(new Uint8Array(encrypted));
}

export async function openSignal(
  key: CryptoKey,
  sequence: number,
  aad: string,
  data: string,
  expectedKind: "offer" | "answer",
): Promise<string> {
  if (!Number.isSafeInteger(sequence) || sequence < 0 || sequence > MAX_SAFE_SEQ)
    throw new Error("信令序号无效");
  const encrypted = decodeBase64Url(data);
  if (encrypted.byteLength > SIGNAL_MAX_BYTES || encodeBase64Url(encrypted) !== data)
    throw new Error("信令数据无效");
  const plain = await crypto.subtle.decrypt(
    {
      name: "AES-GCM",
      iv: makeNonce(sequence),
      additionalData: new TextEncoder().encode(aad),
      tagLength: 128,
    },
    key,
    encrypted,
  );
  if (plain.byteLength > SIGNAL_MAX_BYTES) throw new Error("信令数据过大");
  const value: unknown = JSON.parse(
    new TextDecoder("utf-8", { fatal: true }).decode(plain),
  );
  if (
    typeof value !== "object" || value === null || Array.isArray(value) ||
    Object.keys(value).length !== 2 ||
    !Object.hasOwn(value, "kind") || !Object.hasOwn(value, "sdp")
  )
    throw new Error("信令格式无效");
  const record = value as Record<string, unknown>;
  if (
    record["kind"] !== expectedKind || typeof record["sdp"] !== "string" ||
    !record["sdp"] || record["sdp"].length > SIGNAL_MAX_BYTES
  )
    throw new Error("信令内容无效");
  return record["sdp"];
}

export async function deriveKeys(
  secret: CryptoKey,
  phoneNonce: Uint8Array,
  desktopNonce: Uint8Array,
): Promise<[CryptoKey, CryptoKey]> {
  const salt = new Uint8Array(64);
  salt.set(phoneNonce);
  salt.set(desktopNonce, 32);
  const derive = (info: string): Promise<CryptoKey> =>
    crypto.subtle.deriveKey(
      {
        name: "HKDF",
        hash: "SHA-256",
        salt,
        info: new TextEncoder().encode(info),
      },
      secret,
      { name: "AES-GCM", length: 256 },
      false,
      ["encrypt", "decrypt"],
    );
  return Promise.all([
    derive("fintwind.remote.v1.client"),
    derive("fintwind.remote.v1.server"),
  ]);
}

export function makeNonce(seq: number): Uint8Array<ArrayBuffer> {
  if (!Number.isSafeInteger(seq) || seq < 0 || seq > MAX_SAFE_SEQ)
    throw new Error("加密序号失效");
  const bytes = new Uint8Array(12);
  new DataView(bytes.buffer).setBigUint64(4, BigInt(seq));
  return bytes;
}

export async function sealRecord(
  key: CryptoKey,
  seq: number,
  value: Record<string, unknown>,
): Promise<string> {
  if (seq > MAX_SAFE_SEQ) throw new Error("加密序号耗尽");
  const data = await crypto.subtle.encrypt(
    {
      name: "AES-GCM",
      iv: makeNonce(seq),
      additionalData: new TextEncoder().encode("fintwind.remote.v1"),
      tagLength: 128,
    },
    key,
    new TextEncoder().encode(JSON.stringify(value)),
  );
  return JSON.stringify({
    type: "sealed",
    seq,
    data: encodeBase64Url(new Uint8Array(data)),
  });
}

export async function openEnvelope(
  envelope: Record<string, unknown>,
  receiveKey: CryptoKey,
  expectedSeq: number,
): Promise<unknown> {
  if (
    Object.keys(envelope).length !== 3 ||
    envelope["type"] !== "sealed" ||
    envelope["seq"] !== expectedSeq ||
    !Number.isSafeInteger(envelope["seq"]) ||
    (envelope["seq"] as number) > MAX_SAFE_SEQ ||
    typeof envelope["data"] !== "string" ||
    (envelope["data"] as string).length > FRAME_MAX_BYTES
  ) {
    throw new Error("加密序号无效");
  }
  const text = envelope["data"] as string;
  let encrypted: Uint8Array<ArrayBuffer>;
  try {
    encrypted = decodeBase64Url(text);
  } catch {
    throw new Error("加密数据格式无效");
  }
  if (encodeBase64Url(encrypted) !== text) throw new Error("加密数据格式无效");
  const plain = await crypto.subtle.decrypt(
    {
      name: "AES-GCM",
      iv: makeNonce(envelope["seq"] as number),
      additionalData: new TextEncoder().encode("fintwind.remote.v1"),
      tagLength: 128,
    },
    receiveKey,
    encrypted,
  );
  if (plain.byteLength > RESPONSE_MAX_BYTES) throw new Error("响应数据过大");
  return JSON.parse(
    new TextDecoder("utf-8", { fatal: true }).decode(plain),
  ) as unknown;
}
