const BASE64URL_RE = /^[A-Za-z0-9_-]+$/;
export const MAX_SAFE_SEQ = Number.MAX_SAFE_INTEGER;
export const FRAME_MAX_BYTES = 1024 * 1024;
export const RESPONSE_MAX_BYTES = 512 * 1024;

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

export async function deriveKeys(
  secret: Uint8Array,
  phoneNonce: Uint8Array,
  desktopNonce: Uint8Array,
): Promise<[CryptoKey, CryptoKey]> {
  const material = await crypto.subtle.importKey(
    "raw",
    new Uint8Array(secret),
    "HKDF",
    false,
    ["deriveKey"],
  );
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
      material,
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
