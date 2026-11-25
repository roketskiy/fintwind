import {
  deriveSignalKeys,
  encodeBase64Url,
  openSignal,
  sealSignal,
  sha256Hex,
} from "./remote-crypto";
import {
  BUFFER_LOW_BYTES,
  MessageReassembler,
  MessageSender,
} from "./remote-framing";
import { isRecord } from "./remote-validation";

const SIGNAL_TIMEOUT_MS = 30000;
const DIRECT_TIMEOUT_MS = 30000;
const MAX_SIGNAL_BYTES = 64 * 1024;
const MAX_CHUNK_FRAME_CHARS = 16 * 1024;

export interface DirectConnection {
  attempt: string;
  offerHash: string;
  answerHash: string;
  send(text: string): Promise<void>;
  close(): void;
}

export const SIGNAL_RETRY_MESSAGE = "连接尝试过于频繁，稍后自动重试。";

export class SignalRateLimited extends Error {
  constructor(readonly retryAfterMs: number) {
    super(SIGNAL_RETRY_MESSAGE);
  }
}

interface Options {
  route: string;
  secret: CryptoKey;
  signal: AbortSignal;
  onMessage(text: string): void;
  onDisconnect(error: Error): void;
}

export async function connectDirect(options: Options): Promise<DirectConnection> {
  const routeHash = await sha256Hex(options.route);
  const attemptBytes = crypto.getRandomValues(new Uint8Array(32));
  const attempt = encodeBase64Url(attemptBytes);
  const [phoneKey, desktopKey] = await deriveSignalKeys(options.secret, attemptBytes);
  options.signal.throwIfAborted();
  const socket = new WebSocket(`wss://${window.location.host}/signal/v2`);
  let peer: RTCPeerConnection;
  try {
    peer = new RTCPeerConnection({
      iceServers: [{ urls: `stun:${window.location.hostname}:18762` }],
      iceCandidatePoolSize: 0,
    });
  } catch {
    socket.close();
    throw new Error("无法启动直连");
  }
  let channel: RTCDataChannel;
  try {
    channel = peer.createDataChannel("fintwind.remote.v2", {
      ordered: true,
      protocol: "fintwind.remote.v2",
    });
  } catch {
    peer.close();
    socket.close();
    throw new Error("无法创建数据通道");
  }
  channel.binaryType = "arraybuffer";
  let sender: MessageSender | null = null;
  let receiver: MessageReassembler | null = null;
  let settled = false;
  let peerOnline = false;
  let negotiationStarted = false;
  let signallingReady = false;
  let answerReceived = false;
  let failed = false;
  let timer = 0;
  let resolveReady: (connection: DirectConnection) => void;
  let rejectReady: (error: Error) => void;
  const ready = new Promise<DirectConnection>((resolve, reject) => {
    resolveReady = resolve;
    rejectReady = reject;
  });
  const cleanups: (() => void)[] = [];
  const listen = <K extends keyof WebSocketEventMap>(
    target: WebSocket,
    type: K,
    listener: (event: WebSocketEventMap[K]) => void,
  ): void => {
    target.addEventListener(type, listener);
    cleanups.push(() => target.removeEventListener(type, listener));
  };
  const closeResources = (): void => {
    window.clearTimeout(timer);
    timer = 0;
    receiver?.clear();
    for (const cleanup of cleanups.splice(0)) cleanup();
    try { channel.close(); } catch {}
    try { peer.close(); } catch {}
    try { socket.close(); } catch {}
  };
  const fail = (error: Error): void => {
    if (failed) return;
    failed = true;
    sender?.close(error);
    closeResources();
    if (!settled) {
      settled = true;
      rejectReady(error);
    } else {
      options.onDisconnect(error);
    }
  };
  const armTimeout = (duration: number): void => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => fail(new Error("无法建立直连，请切换网络后重试")), duration);
  };
  const abort = () => fail(new Error("连接已取消"));
  options.signal.addEventListener("abort", abort, { once: true });
  cleanups.push(() => options.signal.removeEventListener("abort", abort));
  armTimeout(SIGNAL_TIMEOUT_MS);
  const sendJson = (value: Record<string, unknown>): void => {
    if (socket.readyState !== WebSocket.OPEN) throw new Error("信令连接不可用");
    socket.send(JSON.stringify(value));
  };
  const waitForGathering = (): Promise<void> => {
    if (peer.iceGatheringState === "complete") return Promise.resolve();
    return new Promise((resolve, reject) => {
      const finish = (error?: Error): void => {
        window.clearTimeout(gatheringTimer);
        peer.removeEventListener("icegatheringstatechange", change);
        if (error) reject(error);
        else resolve();
      };
      const change = (): void => {
        if (peer.iceGatheringState === "complete") finish();
      };
      const gatheringTimer = window.setTimeout(() => {
        finish(new Error("网络地址探测超时"));
      }, 10000);
      peer.addEventListener("icegatheringstatechange", change);
      cleanups.push(() => finish(new Error("连接已取消")));
      change();
    });
  };
  // Local ICE gathering can run while the WebSocket is opening and joining.
  const prepared = (async () => {
    const offer = await peer.createOffer();
    if (!offer.sdp) throw new Error("无法创建直连请求");
    await peer.setLocalDescription(offer);
    if (failed) throw new Error("直连已中断");
    await waitForGathering();
    const localSdp = peer.localDescription?.sdp;
    if (!localSdp || failed) throw new Error("直连已中断");
    const offerHash = await sha256Hex(localSdp);
    const data = await sealSignal(
      phoneKey,
      0,
      `fintwind.direct.v2|${routeHash}|${attempt}|phone`,
      { kind: "offer", sdp: localSdp },
    );
    return { offerHash, data };
  })();
  void prepared.catch((error: unknown) => fail(error instanceof Error ? error : new Error("无法创建直连请求")));
  const negotiate = async (): Promise<void> => {
    if (negotiationStarted) return;
    negotiationStarted = true;
    const { offerHash, data } = await prepared;
    if (failed || !peerOnline) return;
    answerHandler = async (text: string): Promise<void> => {
      const answerSdp = await openSignal(
        desktopKey,
        0,
        `fintwind.direct.v2|${routeHash}|${attempt}|desktop`,
        text,
        "answer",
      );
      if (failed) throw new Error("直连已中断");
      const answerHash = await sha256Hex(answerSdp);
      await peer.setRemoteDescription({ type: "answer", sdp: answerSdp });
      if (failed) throw new Error("直连已中断");
      established = { attempt, offerHash, answerHash };
      openChannel();
    };
    sendJson({ type: "signal", version: 2, attempt, seq: 0, data });
    armTimeout(DIRECT_TIMEOUT_MS);
  };
  let established: { attempt: string; offerHash: string; answerHash: string } | null = null;
  let answerHandler: ((data: string) => Promise<void>) | null = null;
  const openChannel = (): void => {
    if (channel.readyState !== "open" || !established || settled || failed) return;
    const activeSender = new MessageSender({
      get bufferedAmount() { return channel.bufferedAmount; },
      get readyState() { return channel.readyState; },
      send(data) { channel.send(data); },
      setBufferLowThreshold() { channel.bufferedAmountLowThreshold = BUFFER_LOW_BYTES; },
    });
    sender = activeSender;
    receiver = new MessageReassembler(fail);
    const connection: DirectConnection = {
      ...established,
      send(text) {
        if (failed || channel.readyState !== "open")
          return Promise.reject(new Error("连接不可用"));
        return activeSender.send(text);
      },
      close() { fail(new Error("连接已关闭")); },
    };
    settled = true;
    window.clearTimeout(timer);
    timer = 0;
    socket.close();
    resolveReady(connection);
  };
  listen(socket, "open", () => {
    try {
      sendJson({ type: "join", version: 2, routeHash, role: "phone" });
    } catch (error) {
      fail(error instanceof Error ? error : new Error("信令连接失败"));
    }
  });
  listen(socket, "message", (event: MessageEvent) => {
    void (async () => {
      if (failed || settled) return;
      if (typeof event.data !== "string" || event.data.length > MAX_SIGNAL_BYTES)
        throw new Error("信令格式无效");
      const value: unknown = JSON.parse(event.data);
      if (!isRecord(value) || value["version"] !== 2) throw new Error("信令版本无效");
      if (value["type"] === "error") {
        const retry = value["retryAfterMs"];
        if (typeof retry === "number" && Number.isFinite(retry) && retry > 0)
          throw new SignalRateLimited(Math.min(retry, 30000));
        if (value["message"] === "Attempt rate limited") throw new SignalRateLimited(15000);
        throw new Error("信令服务拒绝连接");
      }
      if (!signallingReady) {
        if (value["type"] !== "joined" || Object.keys(value).length !== 2)
          throw new Error("信令握手无效");
        signallingReady = true;
        return;
      }
      if (value["type"] === "peer") {
        if (
          Object.keys(value).length !== 4 || value["role"] !== "desktop" ||
          typeof value["online"] !== "boolean"
        )
          throw new Error("桌面状态无效");
        peerOnline = value["online"];
        if (!peerOnline && !settled) fail(new Error("桌面未连接，请确认桌面已更新并开启远控"));
        else if (peerOnline && signallingReady) await negotiate();
        return;
      }
      if (value["type"] !== "signal") throw new Error("信令类型无效");
      if (value["sender"] === "desktop" && value["attempt"] !== attempt) return;
      if (
        Object.keys(value).length !== 6 || value["sender"] !== "desktop" ||
        value["attempt"] !== attempt || value["seq"] !== 0 ||
        typeof value["data"] !== "string" || !answerHandler || answerReceived
      )
        throw new Error("收到无效或过期信令");
      answerReceived = true;
      await answerHandler(value["data"]);
    })().catch((error: unknown) => {
      fail(error instanceof Error ? error : new Error("信令处理失败"));
    });
  });
  listen(socket, "error", () => {
    if (!settled) fail(new Error("信令连接失败"));
  });
  listen(socket, "close", () => {
    if (failed) return;
    if (!settled) fail(new Error("信令连接已断开"));
  });
  channel.addEventListener("open", openChannel);
  channel.addEventListener("bufferedamountlow", () => sender?.drain());
  channel.addEventListener("message", (event: MessageEvent) => {
    if (failed || !receiver) return;
    try {
      if (typeof event.data !== "string" || event.data.length > MAX_CHUNK_FRAME_CHARS)
        throw new Error("数据通道消息无效");
      const value: unknown = JSON.parse(event.data);
      const text = receiver.accept(value);
      if (text !== null) options.onMessage(text);
    } catch (error) {
      fail(error instanceof Error ? error : new Error("消息分块无效"));
    }
  });
  channel.addEventListener("close", () => fail(new Error("直连已断开")));
  channel.addEventListener("error", () => fail(new Error("直连发生错误")));
  peer.addEventListener("connectionstatechange", () => {
    if (peer.connectionState === "failed")
      fail(new Error("无法建立直连，请切换网络后重试"));
    else if (peer.connectionState === "closed")
      fail(new Error("直连已关闭"));
    else if (peer.connectionState === "disconnected")
      armTimeout(10000);
    else if (peer.connectionState === "connected" && settled) {
      window.clearTimeout(timer);
      timer = 0;
    }
  });
  return ready;
}
