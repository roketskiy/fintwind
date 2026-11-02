import { lazy, Suspense, useEffect, useRef, useState } from "react";
import {
  ArrowLeftIcon,
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  FolderIcon,
  LaptopIcon,
  LockIcon,
  MessageIcon,
  PlusIcon,
  SearchIcon,
  CloseIcon,
} from "@/components/icons";
import { GlideMenu, PixelLoader } from "@/components/beautiful/primitives";
import { GoalCard } from "@/components/goal-card";
import { PhoneSettings } from "@/components/phone-settings";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Drawer,
  DrawerContent,
  DrawerHeader,
  DrawerTitle,
  DrawerTrigger,
} from "@/components/ui/drawer";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { useRemote } from "@/hooks/use-remote";
import type {
  ClientSnapshot,
  SessionStatus,
  SessionSummary,
  Workspace,
  ComposerOptions,
} from "@/lib/types";

const statusLabels: Record<SessionStatus, string> = {
  idle: "已就绪",
  connecting: "准备中",
  working: "进行中",
  waiting: "待处理",
  failed: "出错",
};
const connectionLabels: Record<ClientSnapshot["connection"], string> = {
  idle: "准备连接",
  connecting: "正在连接",
  connected: "已连接",
  reconnecting: "正在重连",
  unpaired: "未配对",
  error: "连接失败",
  offline: "网络离线",
};
const Conversation = lazy(() =>
  import("@/components/conversation").then((module) => ({
    default: module.Conversation,
  })),
);
const Composer = lazy(() =>
  import("@/components/composer").then((module) => ({
    default: module.Composer,
  })),
);

function Status({ status }: { status: SessionStatus }) {
  if (status === "idle") return null;
  return (
    <Badge variant={status === "failed" ? "destructive" : "secondary"}>
      {(status === "working" || status === "connecting") && (
        <Spinner data-icon="inline-start" />
      )}
      {statusLabels[status]}
    </Badge>
  );
}

function ConnectionBadge({
  connection,
}: {
  connection: ClientSnapshot["connection"];
}) {
  return (
    <Badge
      variant={connection === "connected" ? "outline" : "secondary"}
      role="status"
      aria-live="polite"
    >
      {connection === "connected" ? (
        <LockIcon data-icon="inline-start" />
      ) : ["idle", "connecting", "reconnecting"].includes(connection) ? (
        <Spinner data-icon="inline-start" />
      ) : (
        <LaptopIcon data-icon="inline-start" />
      )}
      {connectionLabels[connection]}
    </Badge>
  );
}

function WorkspacePicker({
  workspaces,
  selected,
  onSelect,
}: {
  workspaces: Workspace[];
  selected: string;
  onSelect: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const workspace = workspaces.find((item) => item.id === selected);
  return (
    <Drawer open={open} onOpenChange={setOpen} showSwipeHandle>
      <DrawerTrigger
        render={
          <Button
            variant="ghost"
            className="workspace-trigger"
            disabled={!workspaces.length}
          />
        }
      >
        <FolderIcon data-icon="inline-start" />
        <span className="truncate">{workspace?.name || "工作区"}</span>
        <ChevronDownIcon data-icon="inline-end" />
      </DrawerTrigger>
      <DrawerContent>
        <DrawerHeader>
          <DrawerTitle>工作区</DrawerTitle>
        </DrawerHeader>
        <div className="flex min-h-0 flex-col gap-2 overflow-y-auto p-4 pb-8">
          {workspaces.map((item) => (
            <Button
              key={item.id}
              variant={item.id === selected ? "secondary" : "ghost"}
              className="workspace-option"
              onClick={() => {
                onSelect(item.id);
                setOpen(false);
              }}
              aria-current={item.id === selected ? "true" : undefined}
            >
              <FolderIcon data-icon="inline-start" />
              <span className="truncate">{item.name}</span>
              {item.id === selected && <CheckIcon data-icon="inline-end" />}
            </Button>
          ))}
        </div>
      </DrawerContent>
    </Drawer>
  );
}

function sessionGroups(sessions: SessionSummary[]) {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const yesterday = new Date(today);
  yesterday.setDate(today.getDate() - 1);
  const groups = new Map<string, SessionSummary[]>();
  for (const session of sessions) {
    const date = new Date(session.updatedAt * 1000);
    const label = date >= today ? "今天" : date >= yesterday ? "昨天" : "更早";
    const group = groups.get(label) || [];
    group.push(session);
    groups.set(label, group);
  }
  return [...groups];
}

function sessionTime(updatedAt: number) {
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(updatedAt * 1000));
}

function LoadingList() {
  return (
    <div
      className="flex flex-col gap-3"
      role="status"
      aria-label="正在加载会话"
    >
      <PixelLoader label="正在加载会话" />
      {[0, 1, 2, 3].map((id) => (
        <div className="session-skeleton" key={id}>
          <Skeleton className="size-11 rounded-2xl" />
          <div className="flex flex-1 flex-col gap-3">
            <Skeleton className="h-4 w-3/5" />
            <Skeleton className="h-3 w-2/5" />
          </div>
        </div>
      ))}
    </div>
  );
}

function PairingState({
  connection,
  onReconnect,
}: {
  connection: ClientSnapshot["connection"];
  onReconnect: () => void;
}) {
  const unpaired = connection === "unpaired";
  const connecting =
    connection === "connecting" ||
    connection === "idle" ||
    connection === "reconnecting";
  return (
    <Empty className="connection-empty">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          {connecting ? <Spinner /> : <LaptopIcon />}
        </EmptyMedia>
        <EmptyTitle>
          {unpaired
            ? "连接你的电脑"
            : connecting
              ? "正在连接电脑"
              : "暂时无法连接"}
        </EmptyTitle>
        <EmptyDescription>
          {unpaired
            ? "在电脑上开启手机远控，然后扫描二维码。"
            : connecting
              ? "正在建立加密连接。"
              : "请保持电脑上的 Fintwind 开启，并检查网络连接。"}
        </EmptyDescription>
      </EmptyHeader>
      {!unpaired && !connecting && (
        <EmptyContent>
          <Button variant="outline" onClick={onReconnect}>
            重新连接
          </Button>
        </EmptyContent>
      )}
    </Empty>
  );
}

export function App() {
  const app = useRemote();
  return <RemoteView app={app} />;
}

export function RemoteView({ app, previewOptions }: { app: ReturnType<typeof useRemote>; previewOptions?: ComposerOptions }) {
  const [search, setSearch] = useState("");
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const backRef = useRef<HTMLButtonElement>(null);
  const titleRef = useRef<HTMLHeadingElement>(null);
  const connected = app.connection === "connected";
  const detail = app.detail;
  const sessionPending = app.pendingScopes.includes(`session:${app.sessionId}`);
  const projectPending = app.pendingScopes.includes(`project:${app.projectId}`);
  const disabled = !connected || app.busy || sessionPending;
  const currentSummary = app.sessions.find((item) => item.id === app.sessionId);
  const visibleSessions = app.sessions.filter((session) =>
    (session.title || "未命名会话")
      .toLocaleLowerCase()
      .includes(search.toLocaleLowerCase()),
  );

  useEffect(() => {
    if (app.page === "conversation")
      backRef.current?.focus({ preventScroll: true });
    else titleRef.current?.focus({ preventScroll: true });
  }, [app.page]);

  useEffect(() => {
    setSearch("");
  }, [app.projectId]);

  useEffect(() => {
    const viewport = window.visualViewport;
    if (!viewport) return;
    const update = () => {
      if (viewport.scale === 1)
        document.documentElement.style.setProperty(
          "--app-height",
          `${viewport.height}px`,
        );
    };
    update();
    viewport.addEventListener("resize", update);
    return () => {
      viewport.removeEventListener("resize", update);
      document.documentElement.style.removeProperty("--app-height");
    };
  }, []);

  return (
    <main className="app-shell">
      <section
        className="screen"
        hidden={app.page !== "sessions"}
        aria-label="会话列表"
      >
        <header className="app-header">
          <WorkspacePicker
            workspaces={app.workspaces}
            selected={app.projectId}
            onSelect={app.selectWorkspace}
          />
          <div className="header-actions">
          <PhoneSettings onForget={async () => { await app.forget(); setDrafts({}); }} />
          <Button
            variant="ghost"
            size="icon"
            aria-label="新建会话"
            disabled={
              !connected || app.busy || projectPending || !app.projectId
            }
            onClick={() => void app.createSession()}
          >
            <PlusIcon />
          </Button>
          </div>
        </header>
        <div className="list-toolbar">
          <div className="flex min-w-0 items-center justify-between gap-3">
            <h1 ref={titleRef} tabIndex={-1}>
              会话
            </h1>
            <ConnectionBadge connection={app.connection} />
          </div>
          <InputGroup>
            <InputGroupInput
              aria-label="搜索会话"
              placeholder="搜索会话"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
            <InputGroupAddon>
              <SearchIcon />
            </InputGroupAddon>
            {search && <InputGroupAddon align="inline-end"><Button variant="ghost" size="icon-sm" aria-label="清除搜索" onClick={() => setSearch("")}><CloseIcon /></Button></InputGroupAddon>}
          </InputGroup>
        </div>
        <div className="list-scroll">
          {app.error && app.connection !== "unpaired" && (
            <Alert variant="destructive" className="mb-4">
              <AlertTitle>无法完成操作</AlertTitle>
              <AlertDescription>{app.error}</AlertDescription>
            </Alert>
          )}
          {projectPending && (
            <Alert className="mb-4">
              <AlertTitle>新建结果待确认</AlertTitle>
              <AlertDescription>
                正在刷新会话列表，请勿重复新建。
              </AlertDescription>
            </Alert>
          )}
          {!app.workspaces.length && !connected ? (
            <PairingState
              connection={app.connection}
              onReconnect={app.reconnect}
            />
          ) : app.loading && !app.sessions.length ? (
            <LoadingList />
          ) : !app.workspaces.length ? (
            <Empty>
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <FolderIcon />
                </EmptyMedia>
                <EmptyTitle>没有可用工作区</EmptyTitle>
                <EmptyDescription>
                  请在电脑上登记工作区后重新开启远控。
                </EmptyDescription>
              </EmptyHeader>
            </Empty>
          ) : !visibleSessions.length ? (
            <Empty>
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <MessageIcon />
                </EmptyMedia>
                <EmptyTitle>
                  {search ? "没有匹配的会话" : "最近还没有会话"}
                </EmptyTitle>
                <EmptyDescription>
                  {search
                    ? "试试其他关键词。"
                    : "这里显示过去 72 小时内更新的会话。"}
                </EmptyDescription>
              </EmptyHeader>
              {!search && (
                <EmptyContent>
                  <Button
                    variant="outline"
                    disabled={!connected || app.busy || projectPending}
                    onClick={() => void app.createSession()}
                  >
                    <PlusIcon data-icon="inline-start" />
                    新建会话
                  </Button>
                </EmptyContent>
              )}
            </Empty>
          ) : (
            sessionGroups(visibleSessions).map(([label, sessions]) => (
              <section className="session-group" key={label} aria-label={label}>
                <h2>{label}</h2>
                <GlideMenu className="session-group-items">
                  {sessions.map((session) => (
                    <Button
                      key={session.id}
                      variant="row"
                      className="session-row"
                      data-menu-row
                      onClick={() => app.openSession(session.id)}
                    >
                      <span className="session-symbol"><MessageIcon /></span>
                      <span className="session-row-content">
                        <span className="session-row-title">
                          {session.title || "未命名会话"}
                        </span>
                        <span className="session-row-meta">
                          <time
                            dateTime={new Date(
                              session.updatedAt * 1000,
                            ).toISOString()}
                          >
                            {sessionTime(session.updatedAt)}
                          </time>
                          <span>
                            {session.status === "idle"
                              ? "已就绪"
                              : statusLabels[session.status]}
                          </span>
                        </span>
                      </span>
                      <ChevronRightIcon data-icon="inline-end" />
                    </Button>
                  ))}
                </GlideMenu>
              </section>
            ))
          )}
        </div>
      </section>

      <section
        className="screen conversation-screen"
        hidden={app.page !== "conversation"}
        aria-label="对话"
      >
        <header className="conversation-header">
          <Button
            ref={backRef}
            variant="ghost"
            size="icon"
            aria-label="返回会话列表"
            onClick={app.back}
          >
            <ArrowLeftIcon />
          </Button>
          <div className="conversation-heading">
            <h1>{detail?.title || currentSummary?.title || "新会话"}</h1>
            <span>
              {app.workspaces.find(
                (workspace) => workspace.id === app.projectId,
              )?.name || "工作区"}
            </span>
          </div>
          {detail ? (
            <Status status={detail.status} />
          ) : (
            <ConnectionBadge connection={app.connection} />
          )}
          <PhoneSettings onForget={async () => { await app.forget(); setDrafts({}); }} />
        </header>
        {!connected && (
          <div className="connection-strip" role="status">
            <span>{connectionLabels[app.connection]}</span>
            <Button variant="ghost" size="sm" onClick={app.reconnect}>
              重连
            </Button>
          </div>
        )}
        {app.error && (
          <Alert variant="destructive" className="screen-alert">
            <AlertTitle>无法完成操作</AlertTitle>
            <AlertDescription>{app.error}</AlertDescription>
          </Alert>
        )}
        {detail?.goal && <GoalCard goal={detail.goal} disabled={disabled || !detail.canSend || detail.desktopRequired} onControl={(action) => { void app.mutate("sendPrompt", { sessionId: detail.id, text: `/goal ${action}` }); }} />}
        {detail ? (
          <Suspense
            fallback={
              <div
                className="chat-loading"
                role="status"
                aria-label="正在加载对话"
              >
                <Skeleton className="h-5 w-3/4" />
                <Skeleton className="h-5 w-full" />
              </div>
            }
          >
            <Conversation
              key={detail.id}
              session={detail}
              disabled={!connected || sessionPending}
              busy={app.busy}
              onError={app.setError}
              onPermission={(optionId) => {
                if (detail.permission)
                  void app.mutate("respondPermission", {
                    sessionId: detail.id,
                    requestId: detail.permission.requestId,
                    optionId,
                  });
              }}
              onAnswers={(answers) => {
                if (detail.questions)
                  void app.mutate("answerQuestions", {
                    sessionId: detail.id,
                    requestId: detail.questions.requestId,
                    answers,
                  });
              }}
            />
          </Suspense>
        ) : app.loading ? (
          <div className="chat-loading" role="status" aria-label="正在加载对话">
            <Skeleton className="ml-auto h-16 w-2/3 rounded-3xl" />
            <Skeleton className="h-5 w-3/4" />
            <Skeleton className="h-5 w-full" />
            <Skeleton className="h-5 w-5/6" />
          </div>
        ) : (
          <Empty className="flex-1">
            <EmptyHeader>
              <EmptyTitle>会话暂不可用</EmptyTitle>
            </EmptyHeader>
            <EmptyContent>
              <Button
                variant="outline"
                onClick={app.refresh}
                disabled={!connected}
              >
                重新加载
              </Button>
            </EmptyContent>
          </Empty>
        )}
        <div className="composer-status">
          {sessionPending && (
            <p className="composer-notice" role="status">
              操作结果待确认，请勿重复提交。
            </p>
          )}
          {detail?.canSend === false && !sessionPending && (
            <p className="composer-notice">
              {detail.desktopRequired
                ? "请在电脑上继续处理。"
                : "会话内容正在加载。"}
            </p>
          )}
        </div>
        {detail && (
          <Suspense
            fallback={
              <div className="composer-region">
                <Skeleton className="h-20 w-full" />
              </div>
            }
          >
            <Composer
              key={detail.id}
              session={detail}
              draft={drafts[detail.id] || ""}
              previewOptions={previewOptions}
              onDraft={(value) =>
                setDrafts((previous) => ({ ...previous, [detail.id]: value }))
              }
              disabled={disabled || app.page !== "conversation"}
              busy={app.busy}
              onError={app.setError}
              mutate={app.mutate}
            />
          </Suspense>
        )}
      </section>
    </main>
  );
}
