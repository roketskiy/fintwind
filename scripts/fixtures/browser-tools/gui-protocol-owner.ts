/**
 * Phase-three E2E fixture: a **simulated GUI** — a protocol owner that speaks
 * the daemon's `/v1` bridge exactly like the native WebView2 host does, but
 * fabricates page behavior instead of rendering it.
 *
 * It exists to keep the tool path real while the GUI is not. The real parts in
 * the acceptance chain are the daemon, the OpenCode driver, the private
 * `/v1/browser-tools` channel and the broker. This fixture only plays the page
 * owner: it publishes shared pages, answers snapshot/click/fill/scroll/navigate
 * over the real `browserPublish`/`browserRequest`/`browserResult` protocol, and
 * keeps a per-page mutation ledger so the run can prove a mutation reached the
 * owner only after approval.
 *
 * Failure modes this fixture makes observable:
 * - **Mutation before approval**: `pump()` applies nothing while the policy
 *   holds a request, so `appliedCount` stays put until the runner approves.
 * - **Rejected action acting anyway**: a rejected request is answered as an
 *   error and never touches the ledger.
 * - **Owner disconnect re-sent**: nothing here reconnects or replays; a
 *   disconnect is terminal for this owner, and the run asserts the caller did
 *   not retry the action.
 *
 * `snapshotFor` is a runner-supplied callback, so the exact bytes the model is
 * expected to see through the plugin's untrusted wrapper are known to the
 * runner and can be asserted end-to-end.
 *
 * This fixture deliberately does **not** register a browser host: the run
 * proves that `fintwind_browser_open` really reaches the daemon by being
 * refused there when no Fintwind window hosts the session's browser, instead
 * of being answered by a fixture.
 */

import { randomUUID } from 'node:crypto';

type Uuid = string;
type Scope = { sessionId: Uuid; runtimeId: Uuid; pageId: Uuid; grantId: Uuid };
/** One action as it crosses the wire. `click`/`fill` address one element with
 *  the wire's single `selector` field: either an opaque snapshot ref
 *  (`ref:UUID:ordinal`), which the native owner resolves to the exact element
 *  the last snapshot observed, or a CSS selector. `ref` is kept here only so a
 *  recorded action can be inspected either way; nothing sends it. */
export type BrowserAction =
  | { kind: 'snapshot' }
  | { kind: 'click'; ref?: string; selector?: string }
  | { kind: 'fill'; ref?: string; selector?: string; text: string }
  | { kind: 'navigate'; url: string }
  | { kind: 'scroll'; deltaY: number };

type GuiPage = {
  scope: Scope;
  url: string;
  title: string;
  /** How many approved mutations reached this page through the bridge. */
  appliedCount: number;
  /** How many snapshots this owner served. */
  snapshotCount: number;
};

type PendingAction = { requestId: Uuid; scope: Scope; action: BrowserAction };

/** What to do with a mutation request when `pump` runs. */
export type GuiDecision = 'approve' | 'reject' | 'hold';
export type GuiPolicy = (action: BrowserAction, page: GuiPage | undefined) => GuiDecision;

export class GuiProtocolOwner {
  private socket?: WebSocket;
  connected = false;
  error: string | null = null;
  /** Live pages this owner has published, keyed by pageId. */
  readonly pages = new Map<Uuid, GuiPage>();
  /** Requests delivered by the broker and not yet answered. */
  readonly pending = new Map<Uuid, PendingAction>();
  /** Notices the daemon pushed to this owner, for the runner to assert on. */
  readonly notices: Array<{ type: string; [key: string]: unknown }> = [];
  /** Decision policy, settable by the runner between pumps. */
  policy: GuiPolicy = () => 'hold';
  /** Fabricated snapshot payload for a page; the runner configures this. */
  snapshotFor: (page: GuiPage) => Record<string, unknown> = page => ({
    url: page.url,
    title: page.title,
    text: `simulated snapshot of ${page.scope.pageId}`,
    controls: [],
    truncated: false,
  });

  async connect(address: string, token: string, protocolVersion: number, timeoutMs = 10_000): Promise<void> {
    this.socket = new WebSocket(address);
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`the simulated GUI could not reach ${address}`)), timeoutMs);
      this.socket!.onopen = () => { clearTimeout(timer); resolve(); };
      this.socket!.onerror = () => { clearTimeout(timer); reject(new Error(`the simulated GUI socket errored for ${address}`)); };
    });
    this.socket.onmessage = (event: MessageEvent) => this.onMessage(String(event.data));
    this.socket.onclose = () => { this.connected = false; };
    this.socket.send(JSON.stringify({ type: 'hello', protocolVersion, token, clientId: randomUUID(), resumeFrom: [] }));
  }

  private onMessage(data: string): void {
    let message: Record<string, unknown>;
    try {
      message = JSON.parse(data);
    } catch {
      return;
    }
    switch (message.type) {
      case 'hello':
        this.connected = true;
        return;
      case 'browserRequest': {
        const request = message.request as { requestId: Uuid; scope: Scope; action: BrowserAction };
        this.pending.set(request.requestId, { requestId: request.requestId, scope: request.scope, action: request.action });
        return;
      }
      case 'browserCancel':
        this.pending.delete(message.requestId as Uuid);
        this.notices.push({ type: 'browserCancel', requestId: message.requestId });
        return;
      case 'browserScopesRevoked': {
        const scopes = (message.scopes as Scope[]) ?? [];
        for (const scope of scopes) {
          const page = this.pages.get(scope.pageId);
          if (page?.scope.grantId === scope.grantId && page.scope.runtimeId === scope.runtimeId
            && page.scope.sessionId === scope.sessionId) this.pages.delete(scope.pageId);
        }
        for (const id of [...this.pending.keys()]) {
          const action = this.pending.get(id)!;
          if (scopes.some(scope => scope.pageId === action.scope.pageId && scope.grantId === action.scope.grantId)) {
            this.pending.delete(id);
          }
        }
        this.notices.push({ type: 'browserScopesRevoked', scopes: scopes.length });
        return;
      }
      case 'browserShareRejected':
        this.error = String(message.message ?? 'share rejected');
        this.notices.push({ type: 'browserShareRejected' });
        return;
      default:
        return;
    }
  }

  /** Publish (replace) this owner's pages. An empty list revokes them all. */
  sharePages(pages: Array<{ scope: Scope; url: string; title: string }>): void {
    if (!this.socket) throw new Error('the simulated GUI is not connected');
    this.socket.send(JSON.stringify({ type: 'browserPublish', pages }));
    // This is the fixture's proposed view, not proof that publication was
    // accepted. The runner waits for the real broker's BrowserList before
    // making any assertion that the page is shared.
    const published = new Set(pages.map(page => page.scope.pageId));
    for (const id of [...this.pages.keys()]) {
      if (!published.has(id)) this.pages.delete(id);
    }
    for (const page of pages) {
      const previous = this.pages.get(page.scope.pageId);
      this.pages.set(page.scope.pageId, {
        scope: page.scope,
        url: page.url,
        title: page.title,
        appliedCount: previous?.appliedCount ?? 0,
        snapshotCount: previous?.snapshotCount ?? 0,
      });
    }
  }

  /** A page this owner published, or undefined. */
  page(pageId: Uuid): GuiPage | undefined {
    return this.pages.get(pageId);
  }

  /** True while an action of `kind` is pending approval on `pageId`. */
  hasPending(kind: BrowserAction['kind'], pageId?: Uuid): boolean {
    for (const action of this.pending.values()) {
      if (action.action.kind === kind && (pageId === undefined || action.scope.pageId === pageId)) return true;
    }
    return false;
  }

  /** The actions of `kind` still waiting for approval, in delivery order. */
  pendingActions(kind: BrowserAction['kind'], pageId?: Uuid): BrowserAction[] {
    const actions: BrowserAction[] = [];
    for (const action of this.pending.values()) {
      if (action.action.kind === kind && (pageId === undefined || action.scope.pageId === pageId)) {
        actions.push(action.action);
      }
    }
    return actions;
  }

  /** Total approved mutations across all pages (the run's UI counter). */
  totalApplied(): number {
    let total = 0;
    for (const page of this.pages.values()) total += page.appliedCount;
    return total;
  }

  /**
   * Apply the current policy to every pending request exactly once. Held
   * requests stay pending. Returns the decisions actually carried out, so a
   * run can assert "nothing happened yet" by seeing an empty list.
   */
  pump(): Array<{ requestId: Uuid; decision: GuiDecision; action: BrowserAction['kind'] }> {
    const carried: Array<{ requestId: Uuid; decision: GuiDecision; action: BrowserAction['kind'] }> = [];
    for (const action of [...this.pending.values()]) {
      const page = this.pages.get(action.scope.pageId);
      const decision = this.policy(action.action, page);
      if (decision === 'hold') continue;
      if (decision === 'reject') {
        this.answer(action.requestId, { kind: 'error', message: 'rejected by the simulated GUI owner' });
      } else if (action.action.kind === 'snapshot') {
        if (page) page.snapshotCount += 1;
        this.answer(action.requestId, { kind: 'ok', value: this.snapshotFor(page ?? { scope: action.scope, url: '', title: '', appliedCount: 0, snapshotCount: 0 }) });
      } else if (action.action.kind === 'scroll') {
        // Only an approved mutation reaches the page; the ledger is the
        // simulated stand-in for the real control's on-screen count. The
        // value echoes the action that was actually applied.
        if (page) page.appliedCount += 1;
        this.answer(action.requestId, { kind: 'ok', value: { kind: 'scroll', deltaY: action.action.deltaY } });
      } else {
        if (page) page.appliedCount += 1;
        this.answer(action.requestId, { kind: 'ok', value: { applied: true } });
      }
      this.pending.delete(action.requestId);
      carried.push({ requestId: action.requestId, decision, action: action.action.kind });
    }
    return carried;
  }

  private answer(requestId: Uuid, result: { kind: 'ok'; value: unknown } | { kind: 'error'; message: string }): void {
    this.socket?.send(JSON.stringify({ type: 'browserResult', requestId, result }));
  }

  close(): void {
    try {
      this.socket?.close();
    } catch {
      /* Already closed. */
    }
    this.connected = false;
  }
}
