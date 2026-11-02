import * as React from "react";

import "./beautiful/conversation.css";

import {
  DesktopRequiredAlert,
  PermissionCard,
  QuestionsCard,
} from "@/components/approvals";
import {
  CheckIcon,
  ChevronRightIcon,
  CopyIcon,
  MessageIcon,
} from "@/components/icons";
import { Markdown } from "@/components/markdown";
import { Bubble, BubbleContent } from "@/components/ui/bubble";
import {
  ReasoningActivityRow,
  ToolActivityRow,
} from "@/components/beautiful/thinking-state";
import { Button } from "@/components/ui/button";
import { PixelLoader } from "@/components/beautiful/primitives";
import {
  Empty,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Marker, MarkerContent } from "@/components/ui/marker";
import {
  Message,
  MessageContent,
  MessageFooter,
} from "@/components/ui/message";
import {
  MessageScroller,
  MessageScrollerButton,
  MessageScrollerContent,
  MessageScrollerItem,
  MessageScrollerProvider,
  MessageScrollerViewport,
  useMessageScrollerScrollable,
} from "@/components/ui/message-scroller";
import { Trace } from "@/components/ui/trace";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import type {
  RemoteMessage,
  RemoteActivity,
  SessionDetail,
  TranscriptEntry,
} from "@/lib/types";

// Thinking + tool rows live in beautiful/thinking-state.tsx, adapted from
// Beautiful UI ThinkingState.tsx Reasoning variant (MIT, rev 44a274e).
const ActivityRow = React.memo(
  function ActivityRow({ activity }: { activity: RemoteActivity }) {
    if (activity.kind === "reasoning") {
      return <ReasoningActivityRow activity={activity} />;
    }
    return <ToolActivityRow activity={activity} />;
  },
  (previous, next) => {
    const a = previous.activity,
      b = next.activity;
    return (
      a.id === b.id &&
      a.kind === b.kind &&
      a.title === b.title &&
      a.text === b.text &&
      a.complete === b.complete &&
      a.failed === b.failed &&
      a.truncated === b.truncated
    );
  },
);

const ACTIVITY_PAGE_SIZE = 5;

const ActivityBranch = React.memo(function ActivityBranch({ activities }: { activities: RemoteActivity[] }) {
  const working = activities.some((activity) => !activity.complete && !activity.failed);
  const failures = activities.filter((activity) => activity.failed).length;
  const [manualExpanded, setManualExpanded] = React.useState<boolean | null>(null);
  const { end: readingAboveLatest } = useMessageScrollerScrollable();
  const [autoExpanded, setAutoExpanded] = React.useState(working);
  const [range, setRange] = React.useState<{ first: string; last: string; manual: boolean } | null>(null);
  const tailStart = Math.max(0, activities.length - ACTIVITY_PAGE_SIZE);
  const pinnedEnd = range ? activities.findIndex((activity) => activity.id === range.last) : -1;
  const end = pinnedEnd >= 0 ? pinnedEnd + 1 : activities.length;
  const start = pinnedEnd >= 0 && range
    ? Math.max(0, activities.findIndex((activity) => activity.id === range.first)) : tailStart;
  React.useLayoutEffect(() => {
    if (range && pinnedEnd < 0) setRange(null);
    else if (!readingAboveLatest && range && !range.manual) setRange(null);
    else if (readingAboveLatest && !range && activities[tailStart] && activities.at(-1)) {
      setRange({ first: activities[tailStart]!.id, last: activities.at(-1)!.id, manual: false });
    }
  }, [activities, readingAboveLatest, range, pinnedEnd, tailStart]);
  React.useEffect(() => {
    if (working) setAutoExpanded(true);
    else if (!readingAboveLatest) setAutoExpanded(false);
  }, [working, readingAboveLatest]);
  // Do not remove the activity a reader is inspecting when it completes.
  // Normal completion still folds at the bottom; manual choices always win.
  const expanded = manualExpanded ?? (working || (autoExpanded && readingAboveLatest));
  return <Collapsible className="bui-branch" open={expanded} onOpenChange={setManualExpanded}>
    <CollapsibleTrigger className="bui-branch-trigger">
      <ChevronRightIcon className="bui-branch-chevron" />
      <span>{activities.length} 项活动</span>
      <span className="bui-branch-status" data-failed={failures > 0 || undefined}>
        {failures ? `${failures} 项失败` : working ? "进行中" : "已完成"}
      </span>
    </CollapsibleTrigger>
    <CollapsibleContent>
      {(start > 0 || range?.manual || end < activities.length) && <div className="flex items-center gap-2">
        {start > 0 && <Button type="button" variant="ghost" size="sm" className="min-h-11" onClick={() => {
          setRange({ first: activities[Math.max(0, start - ACTIVITY_PAGE_SIZE)]!.id, last: activities[end - 1]!.id, manual: true });
        }}>更早的 {Math.min(ACTIVITY_PAGE_SIZE, start)} 项</Button>}
        {(range?.manual || end < activities.length) && <Button type="button" variant="ghost" size="sm" className="min-h-11" onClick={() => setRange(null)}>最新 {ACTIVITY_PAGE_SIZE} 项</Button>}
      </div>}
      <Trace className="bui-branch-list" role="list">
        {activities.slice(start, end).map((activity) => <div className="bui-branch-item" role="listitem" data-kind={activity.kind} key={activity.id}>
          <ActivityRow activity={activity} />
        </div>)}
      </Trace>
    </CollapsibleContent>
  </Collapsible>;
}, (previous, next) => previous.activities.length === next.activities.length && previous.activities.every((activity, index) => activity === next.activities[index]));

function transcriptGroups(entries: TranscriptEntry[]) {
  const groups: {
    id: string;
    message?: RemoteMessage;
    activities?: RemoteActivity[];
  }[] = [];
  for (const entry of entries) {
    if (entry.type !== "activity")
      groups.push({ id: `message:${entry.id}`, message: entry });
    else {
      const previous = groups.at(-1);
      if (previous?.activities) previous.activities.push(entry);
      else groups.push({ id: previous ? `activity:after:${previous.id}` : "activity:start", activities: [entry] });
    }
  }
  return groups;
}

export interface ConversationProps {
  session: SessionDetail;
  disabled: boolean;
  busy: boolean;
  onPermission: (optionId: string) => void;
  onAnswers: (answers: { questionId: string; answers: string[] }[]) => void;
  onError: (message: string) => void;
}

async function copyText(value: string) {
  await navigator.clipboard.writeText(value);
}

// Cached formatter: one instance for all message rows.
const clockFormatter = new Intl.DateTimeFormat("zh-CN", {
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});

function formatClock(createdAt: number) {
  const date = new Date(createdAt * 1000);
  return { label: clockFormatter.format(date), iso: date.toISOString() };
}

function formatElapsed(totalSeconds: number) {
  const total = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours > 0) {
    return minutes > 0 ? `用时 ${hours}小时${minutes}分` : `用时 ${hours}小时`;
  }
  if (minutes > 0) {
    return seconds > 0 ? `用时 ${minutes}分${seconds}秒` : `用时 ${minutes}分`;
  }
  return `用时 ${seconds}秒`;
}

function MessageCopyButton({
  text,
  onError,
}: {
  text: string;
  onError: (message: string) => void;
}) {
  const [copied, setCopied] = React.useState(false);

  React.useEffect(() => {
    if (!copied) {
      return;
    }

    const timer = window.setTimeout(() => setCopied(false), 2000);
    return () => window.clearTimeout(timer);
  }, [copied]);

  return (
    <>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        className="bui-press min-h-11 min-w-11"
        aria-label={copied ? "已复制" : "复制回答"}
        onClick={() => {
          void copyText(text)
            .then(() => setCopied(true))
            .catch(() => onError("复制失败。"));
        }}
      >
        {copied ? <CheckIcon /> : <CopyIcon />}
      </Button>
      <span aria-live="polite" className="sr-only">
        {copied ? "已复制" : ""}
      </span>
    </>
  );
}

const MessageRow = React.memo(
  function MessageRow({
    message,
    onError,
  }: {
    message: RemoteMessage;
    onError: (message: string) => void;
  }) {
    // Real turn metadata projected by desktop (RemoteMessage.createdAt unix
    // seconds, elapsedSeconds non-negative). Absent fields render nothing.
    const clock =
      typeof message.createdAt === "number" && message.createdAt > 0
        ? formatClock(message.createdAt)
        : null;
    const elapsed =
      message.role === "assistant" &&
      typeof message.elapsedSeconds === "number" &&
      message.elapsedSeconds >= 0
        ? formatElapsed(message.elapsedSeconds)
        : null;

    if (message.role === "user") {
      return (
        <Message align="end" className="user-message">
          <MessageContent>
            <span className="sr-only">你</span>
            <Bubble variant="secondary" align="end">
              <BubbleContent>
                <div className="user-message-text whitespace-pre-wrap">
                  {message.text}
                  {!!message.attachments?.length && (
                    <div className="sent-attachments">
                      {message.attachments.map((file, index) => (
                        <span key={index}>
                          {file.isImage ? "图片" : "文件"} · {file.name}
                        </span>
                      ))}
                    </div>
                  )}
                </div>
              </BubbleContent>
            </Bubble>
            {clock ? (
              <MessageFooter className="bui-msg-footer">
                <time dateTime={clock.iso} className="bui-msg-time">
                  {clock.label}
                </time>
              </MessageFooter>
            ) : null}
          </MessageContent>
        </Message>
      );
    }

    const showFooter = Boolean(message.text || clock || elapsed);
    return (
      <Message align="start" className="assistant-message">
        <MessageContent>
          <span className="sr-only">助手</span>
          <Bubble variant="ghost" align="start" className="w-full">
            <BubbleContent className="w-full">
              <Markdown text={message.text} />
            </BubbleContent>
          </Bubble>
          {showFooter ? (
            <MessageFooter className="bui-msg-footer">
              {clock ? (
                <time dateTime={clock.iso} className="bui-msg-time">
                  {clock.label}
                </time>
              ) : null}
              {elapsed ? (
                <span className="bui-msg-time">{elapsed}</span>
              ) : null}
              {message.text ? (
                <MessageCopyButton text={message.text} onError={onError} />
              ) : null}
            </MessageFooter>
          ) : null}
        </MessageContent>
      </Message>
    );
  },
  (previous, next) => {
    const a = previous.message,
      b = next.message;
    const aa = a.attachments ?? [],
      ba = b.attachments ?? [];
    return (
      previous.onError === next.onError &&
      a.id === b.id &&
      a.role === b.role &&
      a.text === b.text &&
      a.createdAt === b.createdAt &&
      a.elapsedSeconds === b.elapsedSeconds &&
      aa.length === ba.length &&
      aa.every(
        (file, index) =>
          file.name === ba[index]?.name && file.isImage === ba[index]?.isImage,
      )
    );
  },
);

export function Conversation({
  session,
  disabled,
  busy,
  onPermission,
  onAnswers,
  onError,
}: ConversationProps) {
  const pending = session.permission !== null || session.questions !== null;
  const desktopBlocked = session.desktopRequired;
  const empty =
    (session.entries ?? session.messages).length === 0 &&
    !session.truncated &&
    !pending &&
    !desktopBlocked &&
    !["working", "connecting"].includes(session.status);

  if (empty) {
    return (
      <div className="conversation-body flex min-h-0 min-w-0 flex-1 flex-col">
        <Empty>
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <MessageIcon />
            </EmptyMedia>
            <EmptyTitle>开始新的对话</EmptyTitle>
          </EmptyHeader>
        </Empty>
      </div>
    );
  }

  return (
    <div className="conversation-body flex min-h-0 min-w-0 flex-1 flex-col">
      <MessageScrollerProvider autoScroll defaultScrollPosition="end">
        <MessageScroller>
          <MessageScrollerViewport aria-label="消息" preserveScrollOnPrepend>
            <MessageScrollerContent className="conversation-content">
              {session.truncated ? (
                <MessageScrollerItem
                  key="system:truncated"
                  messageId="system:truncated"
                >
                  <Marker variant="separator">
                    <MarkerContent>更早的消息已省略</MarkerContent>
                  </Marker>
                </MessageScrollerItem>
              ) : null}

              {transcriptGroups(session.entries ?? session.messages).map(
                (group) => (
                  <MessageScrollerItem
                    key={group.id}
                    messageId={group.id}
                  >
                    {group.message ? (
                      <MessageRow
                        message={group.message}
                        onError={onError}
                      />
                    ) : (
                      <ActivityBranch activities={group.activities ?? []} />
                    )}
                  </MessageScrollerItem>
                ),
              )}

              {desktopBlocked ? (
                <MessageScrollerItem
                  key="system:desktop-required"
                  messageId="system:desktop-required"
                >
                  <DesktopRequiredAlert />
                </MessageScrollerItem>
              ) : (
                <>
                  {session.permission ? (
                    <MessageScrollerItem
                      key={`permission:${session.permission.requestId}`}
                      messageId={`permission:${session.permission.requestId}`}
                      className="approval-surface-item"
                    >
                      <PermissionCard
                        permission={session.permission}
                        disabled={disabled}
                        busy={busy}
                        onPermission={onPermission}
                      />
                    </MessageScrollerItem>
                  ) : null}

                  {session.questions ? (
                    <MessageScrollerItem
                      key={`questions:${session.questions.requestId}`}
                      messageId={`questions:${session.questions.requestId}`}
                      className="approval-surface-item"
                    >
                      <QuestionsCard
                        questions={session.questions}
                        disabled={disabled}
                        busy={busy}
                        onAnswers={onAnswers}
                      />
                    </MessageScrollerItem>
                  ) : null}
                </>
              )}

              {session.status === "working" ? (
                <MessageScrollerItem
                  key="system:working"
                  messageId="system:working"
                >
                  <Marker>
                    <MarkerContent><PixelLoader label="正在处理" /></MarkerContent>
                  </Marker>
                </MessageScrollerItem>
              ) : null}
            </MessageScrollerContent>
          </MessageScrollerViewport>
          <MessageScrollerButton />
        </MessageScroller>
      </MessageScrollerProvider>
    </div>
  );
}
