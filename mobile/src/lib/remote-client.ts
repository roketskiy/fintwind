import type {
  ClientSnapshot,
  CommandMap,
  CommandType,
  ConnectionPhase,
  SessionDetail,
} from "./types";
import { forgetPairing, loadPairing, rememberPairing, type SavedPairing } from "./pairing-store";
import {
  MAX_SAFE_SEQ,
  decodeBase64Url,
  deriveKeys,
  encodeBase64Url,
  openEnvelope,
  sealRecord,
  validToken,
} from "./remote-crypto";
import {
  REVISION_RE,
  checkFieldBounds,
  isRecord,
  mutationScope,
  parseResult,
  textBytes,
} from "./remote-validation";
import { SessionUpdates } from "./session-updates";

const UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const COMMAND_TIMEOUT_MS = 120000;
const HANDSHAKE_TIMEOUT_MS = 15000;
const RECONNECT_BASE_MS = 1500;
const RECONNECT_MAX_MS = 30000;
const READ_CACHE_MAX_ITEMS = 8;
const READ_CACHE_MAX_BYTES = 2 * 1024 * 1024;
const READ_CACHE_ITEM_MAX_BYTES = 512 * 1024;
const FRAME_MAX_BYTES = 1024 * 1024;
const UNRESOLVED_MAX_ITEMS = 512;
const PAIR_KEY = "fintwind-pair";
const UNRESOLVED_PREFIX = "fintwind-unresolved";
const PROJECT_KEY = "fintwind-project";
const SESSION_KEY = "fintwind-session";
const TIMEOUT_MESSAGE = "操作结果待确认，请勿重复提交。";
const INTERRUPTED_MESSAGE = "连接中断，操作结果待确认。";

type Listener = () => void;
interface PendingOperation {
  id: string;
  at: number;
  epoch: number;
}
interface InflightItem {
  resolve: (value: never) => void;
  reject: (error: Error) => void;
  timer: number;
  scope: string;
  type: CommandType;
  fields: { projectId?: string; sessionId?: string };
  cacheKey: string;
  cached: unknown;
  startEpoch: number;
}
interface CachedRead {
  data: Record<string, unknown>;
  bytes: number;
}

export class RemoteClient {
  private listeners = new Set<Listener>();
  private snapshot: ClientSnapshot = {
    connection: "idle",
    error: null,
    pendingScopes: [],
  };
  private started = false;
  private ws: WebSocket | null = null;
  private sendKey: CryptoKey | null = null;
  private receiveKey: CryptoKey | null = null;
  private phoneNonce: Uint8Array | null = null;
  private sendSeq = 0;
  private receiveSeq = 0;
  private sendQueue: Promise<void> = Promise.resolve();
  private receiveQueue: Promise<void> = Promise.resolve();
  private transportGeneration = 0;
  private epochCounter = 0;
  private route = "";
  private secret: CryptoKey | null = null;
  private pairPromise: Promise<SavedPairing> | null = null;
  private forgetting = false;
  private lease: { route: string; release: () => void } | null = null;
  private ready = false;
  private inflight = new Map<string, InflightItem>();
  private unresolved = new Map<string, PendingOperation>();
  private readCache = new Map<string, CachedRead>();
  private readCacheBytes = 0;
  private reconnectTimer = 0;
  private handshakeTimer = 0;
  private reconnectDelay = RECONNECT_BASE_MS;
  private onlineHandler: (() => void) | null = null;
  private offlineHandler: (() => void) | null = null;
  private storageHandler: ((event: StorageEvent) => void) | null = null;
  private sessionWatch: { id: string; updates: SessionUpdates; receive: (detail: SessionDetail) => void; fail: (error: Error) => void } | null = null;

  watchSession(sessionId: string, receive: (detail: SessionDetail) => void, fail: (error: Error) => void): () => void {
    const id = crypto.randomUUID();
    const watch = { id, updates: new SessionUpdates(sessionId), receive, fail };
    this.sessionWatch = watch;
    void this.command("watchSession", { sessionId, watchId: id, enabled: true }).catch((error: unknown) => {
      if (this.sessionWatch === watch) {
        this.sessionWatch = null;
        fail(error instanceof Error ? error : new Error("无法订阅会话。"));
      }
    });
    return () => {
      if (this.sessionWatch === watch) this.sessionWatch = null;
      if (this.ready) void this.command("watchSession", { sessionId, watchId: id, enabled: false }).catch(() => {});
    };
  }

  subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  getSnapshot = (): ClientSnapshot => this.snapshot;

  start(): void {
    if (this.started || this.forgetting) return;
    this.started = true;
    this.onlineHandler = () => {
      if (!this.started) return;
      if (this.ready && this.ws?.readyState === WebSocket.OPEN) {
        if (this.snapshot.connection !== "connected")
          this.setPhase("connected", null);
        return;
      }
      this.reconnectDelay = RECONNECT_BASE_MS;
      this.connectNow();
    };
    this.offlineHandler = () => {
      if (!this.started) return;
      this.setPhase("offline", "网络已断开，等待恢复。");
    };
    window.addEventListener("online", this.onlineHandler);
    window.addEventListener("offline", this.offlineHandler);
    this.storageHandler = (event) => {
      if (event.storageArea !== localStorage || (event.key !== "fintwind-paired-route" && event.key !== null)) return;
      if (event.newValue === this.route) return;
      this.stop();
      this.route = "";
      this.secret = null;
      this.pairPromise = null;
      this.readCache.clear();
      this.readCacheBytes = 0;
      this.unresolved.clear();
      this.setPhase("unpaired", "配对已移除或更新，请重新打开配对页面。");
    };
    window.addEventListener("storage", this.storageHandler);
    this.connectNow();
  }

  stop(): void {
    if (!this.started && this.ws === null && this.reconnectTimer === 0) return;
    this.started = false;
    this.sessionWatch = null;
    this.lease?.release();
    this.lease = null;
    this.transportGeneration += 1;
    this.ready = false;
    this.sendKey = null;
    this.receiveKey = null;
    this.phoneNonce = null;
    window.clearTimeout(this.reconnectTimer);
    window.clearTimeout(this.handshakeTimer);
    this.reconnectTimer = 0;
    this.handshakeTimer = 0;
    if (this.onlineHandler)
      window.removeEventListener("online", this.onlineHandler);
    if (this.offlineHandler)
      window.removeEventListener("offline", this.offlineHandler);
    this.onlineHandler = null;
    this.offlineHandler = null;
    if (this.storageHandler) window.removeEventListener("storage", this.storageHandler);
    this.storageHandler = null;
    this.interruptInflight();
    const owned = this.ws;
    this.ws = null;
    this.sendQueue = Promise.resolve();
    this.receiveQueue = Promise.resolve();
    try {
      owned?.close();
    } catch {
      /* already gone */
    }
    this.setPhase("idle", null);
  }

  reconnect(): void {
    if (!this.started) {
      this.start();
      return;
    }
    this.transportGeneration += 1;
    this.ready = false;
    this.sendKey = null;
    this.receiveKey = null;
    this.phoneNonce = null;
    this.interruptInflight();
    window.clearTimeout(this.reconnectTimer);
    window.clearTimeout(this.handshakeTimer);
    this.reconnectTimer = 0;
    this.handshakeTimer = 0;
    const owned = this.ws;
    this.ws = null;
    try {
      owned?.close();
    } catch {
      /* already gone */
    }
    this.reconnectDelay = RECONNECT_BASE_MS;
    this.connectNow();
  }

  async forget(): Promise<void> {
    if (this.forgetting) return;
    this.forgetting = true;
    this.stop();
    try {
    // Let a pending remember finish before deleting it. Never let it recreate the pairing afterward.
    await this.pairPromise?.catch(() => {});
    await forgetPairing();
    this.clearRouteState(this.route, "");
    this.route = "";
    this.secret = null;
    this.pairPromise = null;
    localStorage.removeItem("fintwind-paired-route");
    localStorage.removeItem("fintwind-page");
    this.setPhase("unpaired", null);
    } finally { this.forgetting = false; }
  }

  private claimBrowser(route: string, generation: number): Promise<boolean> {
    if (this.lease?.route === route) return Promise.resolve(true);
    if (!navigator.locks) return Promise.reject(new Error("此浏览器不支持安全的多页面保护，请更新浏览器。"));
    return new Promise((resolve) => {
      void navigator.locks.request(`fintwind-phone:${route}`, { ifAvailable: true }, async (lock) => {
        if (!lock || generation !== this.transportGeneration || !this.started) { resolve(false); return; }
        await new Promise<void>((release) => { this.lease = { route, release }; resolve(true); });
      }).catch(() => resolve(false));
    });
  }

  async command<K extends CommandType>(
    type: K,
    fields: CommandMap[K]["fields"],
  ): Promise<CommandMap[K]["result"]> {
    if (!this.ready || !this.ws) throw new Error("尚未建立加密连接");
    const fieldView = fields as unknown as {
      projectId?: string;
      sessionId?: string;
    };
    checkFieldBounds(type, fields as unknown as Record<string, unknown>);
    const scope = mutationScope(type, fieldView);
    if (scope && this.unresolved.has(scope))
      throw new Error("操作结果待确认，请先等待会话状态刷新。");
    if (scope && this.unresolved.size >= UNRESOLVED_MAX_ITEMS)
      throw new Error("待确认操作过多，请先刷新会话。");
    const id = crypto.randomUUID();
    if (scope) {
      this.unresolved.set(scope, {
        id,
        at: Date.now(),
        epoch: this.epochCounter,
      });
      if (!this.saveUnresolved()) {
        this.unresolved.delete(scope);
        this.refreshSnapshot();
        throw new Error("无法保存操作状态。请允许网站存储后重试。");
      }
      this.refreshSnapshot();
    }
    const generation = this.transportGeneration;
    const target = this.ws;
    const cacheKey = this.readKey(type, fieldView);
    const cached = cacheKey ? this.readCache.get(cacheKey)?.data : undefined;
    const startEpoch = ++this.epochCounter;
    const pending = new Promise<CommandMap[K]["result"]>((resolve, reject) => {
      const timer = window.setTimeout(() => {
        this.inflight.delete(id);
        const current = scope ? this.unresolved.get(scope) : undefined;
        if (scope && current && current.id === id) {
          current.at = Date.now();
          current.epoch = ++this.epochCounter;
          this.saveUnresolved();
          this.refreshSnapshot();
        }
        reject(new Error(TIMEOUT_MESSAGE));
      }, COMMAND_TIMEOUT_MS);
      this.inflight.set(id, {
        resolve: resolve as (value: never) => void,
        reject,
        timer,
        scope,
        type,
        fields: {
          projectId: fieldView.projectId,
          sessionId: fieldView.sessionId,
        },
        cacheKey,
        cached,
        startEpoch,
      });
    });
    void pending.catch(() => {});
    try {
      const sending = this.sendQueue.then(async () => {
        if (generation !== this.transportGeneration || !this.ready)
          throw new Error("连接已改变，操作结果待确认");
        const request: Record<string, unknown> = {
          id,
          command: { type, ...(fields as unknown as Record<string, unknown>) },
        };
        const cachedRevision = isRecord(cached)
          ? cached["revision"]
          : undefined;
        if (cacheKey && typeof cachedRevision === "string")
          request["ifRevision"] = cachedRevision;
        if (this.sendKey === null) throw new Error("尚未建立加密连接");
        if (this.sendSeq > MAX_SAFE_SEQ) throw new Error("加密序号耗尽");
        const seq = this.sendSeq++;
        const envelope = await sealRecord(this.sendKey, seq, request);
        if (generation !== this.transportGeneration || !this.ready)
          throw new Error("连接已改变，操作结果待确认");
        this.sendRaw(envelope, target);
      });
      this.sendQueue = sending.catch(() => {});
      await sending;
    } catch (error) {
      const item = this.inflight.get(id);
      if (item) {
        window.clearTimeout(item.timer);
        item.reject(error instanceof Error ? error : new Error("操作失败"));
      }
      this.inflight.delete(id);
      const unresolved = this.unresolved.get(scope);
      if (unresolved) {
        unresolved.epoch = ++this.epochCounter;
        this.saveUnresolved();
      }
      if (
        generation === this.transportGeneration &&
        target === this.ws &&
        target.readyState === WebSocket.OPEN
      ) {
        try {
          target.close();
        } catch {
          /* already failing */
        }
      }
    }
    return pending;
  }

  private readKey(
    type: CommandType,
    fields: { projectId?: string; sessionId?: string },
  ): string {
    if (mutationScope(type, fields)) return "";
    return JSON.stringify([
      this.route,
      type,
      fields.projectId ?? "",
      fields.sessionId ?? "",
    ]);
  }

  private cacheRead(key: string, data: Record<string, unknown>): void {
    const revision = data["revision"];
    if (typeof revision !== "string" || !REVISION_RE.test(revision)) return;
    const bytes = textBytes(JSON.stringify(data));
    if (bytes > READ_CACHE_ITEM_MAX_BYTES) return;
    const existing = this.readCache.get(key);
    if (existing) {
      this.readCacheBytes -= existing.bytes;
      this.readCache.delete(key);
    }
    while (
      this.readCache.size >= READ_CACHE_MAX_ITEMS ||
      this.readCacheBytes + bytes > READ_CACHE_MAX_BYTES
    ) {
      const oldest = this.readCache.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      const removed = this.readCache.get(oldest);
      if (removed) this.readCacheBytes -= removed.bytes;
      this.readCache.delete(oldest);
    }
    this.readCache.set(key, { data, bytes });
    this.readCacheBytes += bytes;
  }

  private unresolvedKey(route: string): string {
    return `${UNRESOLVED_PREFIX}:${route}`;
  }

  private loadUnresolved(route: string): Map<string, PendingOperation> {
    const next = new Map<string, PendingOperation>();
    const parse = (text: string | null): void => {
      if (!text) return;
      const saved = JSON.parse(text) as unknown;
      if (!Array.isArray(saved)) throw new Error("保存的操作状态无效，请重新配对。");
      for (const entry of saved.slice(0, UNRESOLVED_MAX_ITEMS)) {
        if (!Array.isArray(entry) || typeof entry[0] !== "string") continue;
        const value = entry[1] as { id?: unknown; at?: unknown } | null;
        if (!value || typeof value.id !== "string" || !UUID_RE.test(value.id))
          continue;
        const at =
          typeof value.at === "number" && Number.isFinite(value.at)
            ? value.at
            : Date.now();
        next.set(entry[0], { id: value.id, at, epoch: 0 });
        if (next.size >= UNRESOLVED_MAX_ITEMS) break;
      }
    };
    try {
    const persisted = localStorage.getItem(this.unresolvedKey(route));
    if (persisted !== null) parse(persisted);
    else {
      parse(sessionStorage.getItem(this.unresolvedKey(route)));
      if (next.size === 0) parse(sessionStorage.getItem(UNRESOLVED_PREFIX));
      localStorage.setItem(this.unresolvedKey(route), JSON.stringify([...next]));
    }
    sessionStorage.removeItem(this.unresolvedKey(route));
    sessionStorage.removeItem(UNRESOLVED_PREFIX);
    } catch { throw new Error("无法读取保存的操作状态。请在设置中忘记这台电脑，再重新扫码。"); }
    return next;
  }

  private saveUnresolved(): boolean {
    if (!this.route) return true;
    try {
      localStorage.setItem(
        this.unresolvedKey(this.route),
        JSON.stringify([...this.unresolved]),
      );
      return true;
    } catch {
      return false;
    }
  }

  private clearRouteState(previous: string, next: string): void {
    this.readCache.clear();
    this.readCacheBytes = 0;
    this.unresolved.clear();
    try {
      if (previous) sessionStorage.removeItem(this.unresolvedKey(previous));
      if (previous) localStorage.removeItem(this.unresolvedKey(previous));
      sessionStorage.removeItem(UNRESOLVED_PREFIX);
      sessionStorage.removeItem(PROJECT_KEY);
      sessionStorage.removeItem(SESSION_KEY);
      localStorage.removeItem(PROJECT_KEY);
      localStorage.removeItem(SESSION_KEY);
      localStorage.removeItem("fintwind-page");
    } catch {
      /* memory state is already clear */
    }
    this.unresolved = this.loadUnresolved(next);
    this.refreshSnapshot();
  }

  private pairing(): Promise<SavedPairing> {
    return this.pairPromise ??= this.restorePairing();
  }

  private async restorePairing(): Promise<SavedPairing> {
    const values = new URLSearchParams(window.location.hash.slice(1));
    if (window.location.hash)
      window.history.replaceState(
        window.history.state,
        "",
        window.location.pathname + window.location.search,
      );
    if (values.get("v") === "1" && values.has("route") && values.has("key")) {
      const keys = Array.from(values.keys());
      if (
        keys.length !== 3 ||
        new Set(keys).size !== 3 ||
        !["v", "route", "key"].every((key) => values.has(key))
      ) {
        throw new Error("配对链接无效");
      }
      const next = {
        route: values.get("route") ?? "",
        key: values.get("key") ?? "",
      };
      if (!validToken(next.route) || !validToken(next.key))
        throw new Error("配对链接无效");
      const previous = localStorage.getItem("fintwind-paired-route");
      const pair = await rememberPairing(next.route, next.key);
      if (previous !== next.route) this.clearRouteState(previous || "", next.route);
      localStorage.setItem("fintwind-paired-route", next.route);
      sessionStorage.removeItem(PAIR_KEY);
      return pair;
    }
    const remembered = await loadPairing();
    if (remembered) return remembered;
    let saved: string | null = null;
    try {
      saved = sessionStorage.getItem(PAIR_KEY);
    } catch {
      throw new Error("配对信息无效");
    }
    if (!saved) {
      const error = new Error("请使用桌面生成的配对链接");
      error.name = "MissingPairing";
      throw error;
    }
    let next: unknown;
    try {
      next = JSON.parse(saved) as unknown;
    } catch {
      throw new Error("配对信息无效");
    }
    if (
      !isRecord(next) ||
      !validToken(next["route"]) ||
      !validToken(next["key"])
    )
      throw new Error("配对信息无效");
    const pair = await rememberPairing(next["route"], next["key"]);
    localStorage.setItem("fintwind-paired-route", pair.route);
    sessionStorage.removeItem(PAIR_KEY);
    return pair;
  }

  private sendRaw(value: string, target: WebSocket | null): void {
    if (!target || target !== this.ws || target.readyState !== WebSocket.OPEN)
      throw new Error("连接不可用");
    if (target.bufferedAmount > FRAME_MAX_BYTES) throw new Error("连接不可用");
    target.send(value);
  }

  private settleMutation(scope: string, id: string): void {
    if (scope && this.unresolved.get(scope)?.id === id) {
      this.unresolved.delete(scope);
      this.saveUnresolved();
      this.refreshSnapshot();
    }
  }

  private observeRead(item: InflightItem): void {
    const scope =
      item.type === "getSession"
        ? `session:${item.fields.sessionId ?? ""}`
        : item.type === "listSessions"
          ? `project:${item.fields.projectId ?? ""}`
          : "";
    if (!scope) return;
    const pending = this.unresolved.get(scope);
    const mutationInFlight =
      pending &&
      [...this.inflight.values()].some((request) => request.scope === scope);
    if (pending && !mutationInFlight && item.startEpoch > pending.epoch) {
      this.unresolved.delete(scope);
      this.saveUnresolved();
      this.refreshSnapshot();
    }
  }

  private setPhase(connection: ConnectionPhase, error: string | null): void {
    if (
      this.snapshot.connection === connection &&
      this.snapshot.error === error &&
      this.pendingMatches(this.snapshot.pendingScopes)
    )
      return;
    this.snapshot = {
      connection,
      error,
      pendingScopes: [...this.unresolved.keys()],
    };
    this.emit();
  }

  private refreshSnapshot(): void {
    if (this.pendingMatches(this.snapshot.pendingScopes)) return;
    this.snapshot = {
      connection: this.snapshot.connection,
      error: this.snapshot.error,
      pendingScopes: [...this.unresolved.keys()],
    };
    this.emit();
  }

  private pendingMatches(scopes: readonly string[]): boolean {
    if (scopes.length !== this.unresolved.size) return false;
    for (const scope of scopes) {
      if (!this.unresolved.has(scope)) return false;
    }
    return true;
  }

  private emit(): void {
    for (const listener of this.listeners) listener();
  }

  private failCurrentWs(ws: WebSocket, message: string): void {
    this.ready = false;
    this.setPhase("error", message);
    try {
      ws.close();
    } catch {
      /* close handler reports */
    }
  }

  private async connectNow(): Promise<void> {
    if (!this.started) return;
    if (window.location.protocol !== "https:") {
      this.setPhase("error", "远程控制需要 HTTPS");
      return;
    }
    if (
      typeof window.navigator !== "undefined" &&
      window.navigator.onLine === false
    ) {
      this.setPhase("offline", "网络已断开，等待恢复。");
    } else {
      this.setPhase(this.route ? "reconnecting" : "connecting", null);
    }
    const generation = ++this.transportGeneration;
    this.interruptInflight();
    this.ready = false;
    this.sendKey = null;
    this.receiveKey = null;
    this.phoneNonce = null;
    this.sendSeq = 0;
    this.receiveSeq = 0;
    this.sendQueue = Promise.resolve();
    this.receiveQueue = Promise.resolve();
    let pair: SavedPairing;
    try {
      pair = await this.pairing();
      if (generation !== this.transportGeneration || !this.started) return;
      if (!(await this.claimBrowser(pair.route, generation))) {
        if (generation === this.transportGeneration && this.started) this.setPhase("error", "此电脑已在另一个页面打开，请返回原页面。");
        return;
      }
      if (generation !== this.transportGeneration || !this.started) return;
      if (this.route && this.route !== pair.route) this.clearRouteState(this.route, pair.route);
      this.route = pair.route;
      this.secret = pair.material;
      if (this.unresolved.size === 0) {
        this.unresolved = this.loadUnresolved(this.route);
        this.refreshSnapshot();
      }
    } catch (error) {
      if (generation !== this.transportGeneration || !this.started) return;
      this.lease?.release();
      this.lease = null;
      const message = error instanceof Error ? error.message : "配对信息无效";
      this.setPhase(
        error instanceof Error && error.name === "MissingPairing"
          ? "unpaired"
          : "error",
        message,
      );
      return;
    }
    if (generation !== this.transportGeneration || !this.started) return;
    const previous = this.ws;
    this.ws = null;
    if (previous) {
      try {
        previous.close();
      } catch {
        /* already gone */
      }
    }
    let ws: WebSocket;
    try {
      ws = new WebSocket(
        `wss://${window.location.host}/c/${encodeURIComponent(this.route)}/__fintwind_remote__`,
      );
    } catch {
      this.setPhase("error", "连接失败");
      this.scheduleReconnect(this.ws);
      return;
    }
    this.ws = ws;
    window.clearTimeout(this.handshakeTimer);
    this.handshakeTimer = window.setTimeout(() => {
      if (
        this.ws === ws &&
        !this.ready &&
        generation === this.transportGeneration
      )
        ws.close();
    }, HANDSHAKE_TIMEOUT_MS);
    ws.addEventListener("open", () => {
      if (generation !== this.transportGeneration || this.ws !== ws) return;
      try {
        const nonceBytes = crypto.getRandomValues(new Uint8Array(32));
        this.phoneNonce = nonceBytes;
        this.sendRaw(
          JSON.stringify({
            type: "hello",
            version: 1,
            nonce: encodeBase64Url(nonceBytes),
          }),
          ws,
        );
      } catch {
        try {
          ws.close();
        } catch {
          /* close handler reports */
        }
        return;
      }
      this.setPhase("connecting", null);
    });
    ws.addEventListener("message", (event: MessageEvent) => {
      const pending = this.receiveQueue.then(() =>
        this.processMessage(event, ws, generation),
      );
      this.receiveQueue = pending.catch(() => {});
    });
    ws.addEventListener("close", () => {
      if (this.ws !== ws || generation !== this.transportGeneration) return;
      window.clearTimeout(this.handshakeTimer);
      this.transportGeneration += 1;
      this.ready = false;
      this.sendKey = null;
      this.receiveKey = null;
      this.phoneNonce = null;
      this.interruptInflight();
      if (!this.started) return;
      if (
        typeof window.navigator !== "undefined" &&
        window.navigator.onLine === false
      ) {
        this.setPhase("offline", "网络已断开，等待恢复。");
      } else {
        this.setPhase("reconnecting", this.snapshot.error);
      }
      this.scheduleReconnect(ws);
    });
    ws.addEventListener("error", () => {
      if (
        this.ws === ws &&
        generation === this.transportGeneration &&
        !this.ready
      )
        this.setPhase("connecting", "连接失败");
    });
  }

  private scheduleReconnect(previous: WebSocket | null): void {
    if (!this.started) return;
    window.clearTimeout(this.reconnectTimer);
    const delay = this.reconnectDelay;
    this.reconnectDelay = Math.min(delay * 2, RECONNECT_MAX_MS);
    this.reconnectTimer = window.setTimeout(() => {
      if (!this.started) return;
      if (previous !== null && this.ws !== previous && this.ws !== null) return;
      this.connectNow();
    }, delay);
  }

  private interruptInflight(): void {
    this.sessionWatch = null;
    for (const item of this.inflight.values()) {
      window.clearTimeout(item.timer);
      const pending = this.unresolved.get(item.scope);
      if (pending) {
        pending.at = Date.now();
        pending.epoch = ++this.epochCounter;
      }
      item.reject(new Error(INTERRUPTED_MESSAGE));
    }
    this.inflight.clear();
    this.saveUnresolved();
    this.refreshSnapshot();
  }

  private async processMessage(
    event: MessageEvent,
    ws: WebSocket,
    generation: number,
  ): Promise<void> {
    try {
      if (generation !== this.transportGeneration || this.ws !== ws) return;
      if (typeof event.data !== "string" || event.data.length > FRAME_MAX_BYTES)
        throw new Error("收到无效数据");
      const value = JSON.parse(event.data) as unknown;
      if (!isRecord(value)) throw new Error("收到无效数据");
      if (!this.sendKey || !this.receiveKey) {
        if (
          Object.keys(value).length !== 3 ||
          value["type"] !== "hello" ||
          value["version"] !== 1 ||
          typeof value["nonce"] !== "string"
        ) {
          throw new Error("握手无效");
        }
        let desktopNonce: Uint8Array;
        try {
          desktopNonce = decodeBase64Url(value["nonce"]);
        } catch {
          throw new Error("握手无效");
        }
        if (
          desktopNonce.length !== 32 ||
          encodeBase64Url(desktopNonce) !== value["nonce"]
        )
          throw new Error("握手无效");
        if (!this.secret || !this.phoneNonce) throw new Error("握手无效");
        const derived = await deriveKeys(
          this.secret,
          this.phoneNonce,
          desktopNonce,
        );
        if (generation !== this.transportGeneration || this.ws !== ws) return;
        this.sendKey = derived[0];
        this.receiveKey = derived[1];
        return;
      }
      const expectedSeq = this.receiveSeq;
      const response = await openEnvelope(value, this.receiveKey, expectedSeq);
      if (generation !== this.transportGeneration || this.ws !== ws) return;
      this.receiveSeq = expectedSeq + 1;
      if (!isRecord(response)) throw new Error("响应格式无效");
      if (!this.ready) {
        if (Object.keys(response).length !== 1 || response["type"] !== "ready")
          throw new Error("连接校验失败");
        window.clearTimeout(this.handshakeTimer);
        this.ready = true;
        this.reconnectDelay = RECONNECT_BASE_MS;
        this.setPhase("connected", null);
        return;
      }
      if (response["type"] === "sessionUpdate") {
        const watch = this.sessionWatch;
        if (!watch || response["watchId"] !== watch.id) return;
        try {
          if (Object.hasOwn(response, "error")) throw new Error(typeof response["error"] === "string" ? response["error"].slice(0, 512) : "无法读取会话。");
          watch.receive(watch.updates.apply(response));
        } catch (error) {
          this.sessionWatch = null;
          watch.fail(error instanceof Error ? error : new Error("会话增量已失效。"));
        }
        return;
      }
      if (typeof response["id"] !== "string" || !UUID_RE.test(response["id"]))
        throw new Error("响应格式无效");
      if (typeof response["ok"] !== "boolean") throw new Error("响应格式无效");
      const expectedKeys = response["ok"]
        ? ["id", "ok", "data"]
        : ["id", "ok", "error"];
      if (
        Object.keys(response).length !== expectedKeys.length ||
        !expectedKeys.every((key) => Object.hasOwn(response, key))
      ) {
        throw new Error("响应格式无效");
      }
      if (
        !response["ok"] &&
        (typeof response["error"] !== "string" ||
          response["error"].length > 512)
      ) {
        throw new Error("响应格式无效");
      }
      const item = this.inflight.get(response["id"]);
      if (!item) return;
      let data: unknown = response["ok"] ? response["data"] : null;
      let unchanged = false;
      if (response["ok"] && item.cacheKey) {
        const envelope = data as {
          notModified?: unknown;
          revision?: unknown;
        } | null;
        if (isRecord(envelope) && envelope["notModified"] === true) {
          const cached = item.cached as Record<string, unknown> | undefined;
          if (
            !cached ||
            envelope["revision"] !== cached["revision"] ||
            Object.keys(envelope).length !== 2
          ) {
            item.reject(new Error("会话缓存已失效，请重新连接。"));
            const rejected = this.readCache.get(item.cacheKey);
            if (rejected) this.readCacheBytes -= rejected.bytes;
            this.readCache.delete(item.cacheKey);
            throw new Error("响应版本无效");
          }
          data = cached;
          unchanged = true;
        }
      }
      if (response["ok"]) {
        const parsed = unchanged ? data : parseResult(item.type, data);
        if (!unchanged && item.cacheKey && isRecord(parsed))
          this.cacheRead(item.cacheKey, parsed);
        window.clearTimeout(item.timer);
        this.inflight.delete(response["id"]);
        this.settleMutation(item.scope, response["id"]);
        this.observeRead(item);
        item.resolve(parsed as never);
      } else {
        window.clearTimeout(item.timer);
        this.inflight.delete(response["id"]);
        const message = response["error"] as string;
        if (
          !message.includes("待确认") &&
          !message.includes("unknown") &&
          !message.includes("handler stopped")
        ) {
          this.settleMutation(item.scope, response["id"]);
        }
        item.reject(new Error(message.slice(0, 512) || "桌面操作失败"));
      }
    } catch (error) {
      if (this.ws === ws && generation === this.transportGeneration)
        this.failCurrentWs(
          ws,
          error instanceof Error ? error.message : "连接数据无效",
        );
    }
  }
}

export const remoteClient = new RemoteClient();
