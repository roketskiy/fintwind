import * as React from "react";

import "./beautiful/conversation.css";

import {
  DesktopRequiredAlert,
  PermissionCard,
  QuestionsCard,
} from "@/components/approvals";
import {
  CheckIcon,
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
} from "@/components/ui/message-scroller";
import { Trace } from "@/components/ui/trace";
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
      else groups.push({ id: `activity:${entry.id}`, activities: [entry] });
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
    streaming,
    onError,
  }: {
    message: RemoteMessage;
    streaming?: boolean;
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
            {/* Streaming caret adapted from Beautiful UI StreamingText.tsx; no
                demo timers, citations or follow-ups. */}
            <BubbleContent className="w-full">
              <Markdown text={message.text} />
              {streaming ? (
                <span
                  aria-hidden="true"
                  className="bui-streaming-caret"
                />
              ) : null}
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
      previous.streaming === next.streaming &&
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

  const streamingId =
    session.status === "working"
      ? (() => {
          const entries = session.entries ?? session.messages;
          for (let index = entries.length - 1; index >= 0; index -= 1) {
            const entry = entries[index];
            if (entry.type !== "activity" && entry.role === "assistant") {
              return entry.id;
            }
          }
          return null;
        })()
      : null;

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
                    scrollAnchor={group.message?.role === "user"}
                  >
                    {group.message ? (
                      <MessageRow
                        message={group.message}
                        streaming={group.message.id === streamingId}
                        onError={onError}
                      />
                    ) : (
                      <Trace>
                        {group.activities?.map((activity) => (
                          <ActivityRow key={activity.id} activity={activity} />
                        ))}
                      </Trace>
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
                  <Marker aria-live="polite">
                    <MarkerContent>正在处理</MarkerContent>
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
