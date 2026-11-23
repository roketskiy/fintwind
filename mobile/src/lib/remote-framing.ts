import { decodeBase64Url, encodeBase64Url } from "./remote-crypto";
import { isRecord, textBytes } from "./remote-validation";

export const CHUNK_BYTES = 8192;
export const MESSAGE_MAX_BYTES = 1024 * 1024;
export const BUFFER_HIGH_BYTES = 256 * 1024;
export const BUFFER_LOW_BYTES = 64 * 1024;
export const SEND_QUEUE_MAX_ITEMS = 8;
const REASSEMBLY_TIMEOUT_MS = 15000;
const MAX_CHUNKS = Math.ceil(MESSAGE_MAX_BYTES / CHUNK_BYTES);
const ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export interface ChunkFrame {
  type: "chunk";
  id: string;
  index: number;
  total: number;
  data: string;
}

export function encodeChunks(text: string): ChunkFrame[] {
  const bytes = new TextEncoder().encode(text);
  if (!bytes.byteLength || bytes.byteLength > MESSAGE_MAX_BYTES)
    throw new Error("消息大小无效");
  const id = crypto.randomUUID();
  const total = Math.ceil(bytes.byteLength / CHUNK_BYTES);
  const frames: ChunkFrame[] = [];
  for (let index = 0; index < total; index += 1) {
    frames.push({
      type: "chunk",
      id,
      index,
      total,
      data: encodeBase64Url(bytes.subarray(index * CHUNK_BYTES, (index + 1) * CHUNK_BYTES)),
    });
  }
  return frames;
}

interface Reassembly {
  id: string;
  total: number;
  parts: Uint8Array[];
  bytes: number;
}

export class MessageReassembler {
  private current: Reassembly | null = null;
  private timer = 0;
  private readonly expire = () => {
    this.current = null;
    this.timer = 0;
    this.onFailure(new Error("消息接收超时"));
  };

  constructor(private readonly onFailure: (error: Error) => void) {}

  accept(value: unknown): string | null {
    if (
      !isRecord(value) || Object.keys(value).length !== 5 ||
      value["type"] !== "chunk" || typeof value["id"] !== "string" ||
      !ID_RE.test(value["id"]) || !Number.isSafeInteger(value["index"]) ||
      !Number.isSafeInteger(value["total"]) || typeof value["data"] !== "string"
    )
      throw new Error("消息分块无效");
    const id = value["id"];
    const index = value["index"] as number;
    const total = value["total"] as number;
    const data = value["data"];
    const expected = this.current ? this.current.parts.length : 0;
    if (
      index !== expected || index < 0 || total < 1 || index >= total ||
      total > MAX_CHUNKS ||
      (this.current && (this.current.id !== id || this.current.total !== total))
    )
      throw new Error("消息分块顺序无效");
    const bytes = decodeBase64Url(data);
    if (
      !bytes.byteLength || bytes.byteLength > CHUNK_BYTES ||
      (index < total - 1 && bytes.byteLength !== CHUNK_BYTES) ||
      encodeBase64Url(bytes) !== data
    )
      throw new Error("消息分块大小无效");
    if (!this.current) {
      this.current = { id, total, parts: [], bytes: 0 };
      this.timer = window.setTimeout(this.expire, REASSEMBLY_TIMEOUT_MS);
    }
    this.current.bytes += bytes.byteLength;
    if (this.current.bytes > MESSAGE_MAX_BYTES) throw new Error("消息过大");
    this.current.parts.push(bytes);
    if (this.current.parts.length !== total) return null;
    const complete = new Uint8Array(this.current.bytes);
    let offset = 0;
    for (const part of this.current.parts) {
      complete.set(part, offset);
      offset += part.byteLength;
    }
    this.clear();
    return new TextDecoder("utf-8", { fatal: true }).decode(complete);
  }

  clear(): void {
    window.clearTimeout(this.timer);
    this.timer = 0;
    this.current = null;
  }
}

export interface FramedChannel {
  readonly readyState: RTCDataChannelState;
  readonly bufferedAmount: number;
  send(data: string): void;
  setBufferLowThreshold(): void;
}

interface WaitingSend {
  frames: string[];
  next: number;
  resolve: () => void;
  reject: (error: Error) => void;
}

export class MessageSender {
  private queue: WaitingSend[] = [];
  private current: WaitingSend | null = null;
  private closed = false;

  constructor(private readonly channel: FramedChannel) {
    channel.setBufferLowThreshold();
  }

  send(text: string): Promise<void> {
    if (this.closed) return Promise.reject(new Error("连接不可用"));
    if (this.queue.length + (this.current ? 1 : 0) >= SEND_QUEUE_MAX_ITEMS)
      return Promise.reject(new Error("连接繁忙，请稍后重试"));
    if (!text || textBytes(text) > MESSAGE_MAX_BYTES)
      return Promise.reject(new Error("消息过大"));
    const frames = encodeChunks(text).map((frame) => JSON.stringify(frame));
    return new Promise((resolve, reject) => {
      this.queue.push({ frames, next: 0, resolve, reject });
      this.pump();
    });
  }

  drain(): void {
    if (!this.closed) this.pump();
  }

  private pump(): void {
    while (!this.closed) {
      if (!this.current) {
        const next = this.queue.shift();
        if (!next) return;
        this.current = next;
      }
      const item = this.current;
      while (item.next < item.frames.length) {
        if (this.channel.readyState !== "open") {
          this.close(new Error("连接不可用"));
          return;
        }
        if (this.channel.bufferedAmount >= BUFFER_HIGH_BYTES) return;
        try {
          this.channel.send(item.frames[item.next]);
        } catch (error) {
          this.close(error instanceof Error ? error : new Error("连接不可用"));
          return;
        }
        item.next += 1;
      }
      this.current = null;
      item.resolve();
    }
  }

  close(error: Error): void {
    if (this.closed) return;
    this.closed = true;
    const waiting = this.current ? [this.current, ...this.queue] : this.queue;
    this.current = null;
    this.queue = [];
    for (const item of waiting) item.reject(error);
  }
}
