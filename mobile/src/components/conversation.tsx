import * as React from "react";

import {
  DesktopRequiredAlert,
  PermissionCard,
  QuestionsCard,
} from "@/components/approvals";
import {
  CheckIcon,
  CopyIcon,
  MessageIcon,
  LoaderIcon,
  SearchIcon,
} from "@/components/icons";
import { Markdown } from "@/components/markdown";
import { Bubble, BubbleContent } from "@/components/ui/bubble";
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
import {
  Trace,
  TraceLine,
  TraceIcon,
  TraceTitle,
  TraceDescription,
  TraceCollapsible,
  TraceCollapsibleTrigger,
  TraceCollapsibleContent,
} from "@/components/ui/trace";
import type {
  RemoteMessage,
  RemoteActivity,
  SessionDetail,
  TranscriptEntry,
} from "@/lib/types";

const ActivityRow = React.memo(
  function ActivityRow({ activity }: { activity: RemoteActivity }) {
    const reasoning = activity.kind === "reasoning";
    const status = activity.failed
      ? "error"
      : !activity.complete
        ? "running"
        : undefined;
    const line = (
      <TraceLine status={status}>
        <TraceIcon>
          {!activity.complete ? (
            <LoaderIcon />
          ) : reasoning ? (
            <MessageIcon />
          ) : (
            <SearchIcon />
          )}
        </TraceIcon>
        <TraceTitle>
          {reasoning ? "思考" : activity.title || "工具调用"}
        </TraceTitle>
        <TraceDescription>
          {activity.failed ? "失败" : activity.complete ? "完成" : "进行中"}
        </TraceDescription>
      </TraceLine>
    );
    if (!reasoning || !activity.text) return line;
    return (
      <TraceCollapsible>
        <TraceCollapsibleTrigger>{line}</TraceCollapsibleTrigger>
        <TraceCollapsibleContent>
          {activity.truncated && <p>较早的思考已省略</p>}
          <div className="reasoning-text">{activity.text}</div>
        </TraceCollapsibleContent>
      </TraceCollapsible>
    );
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
          </MessageContent>
        </Message>
      );
    }

    return (
      <Message align="start" className="assistant-message">
        <MessageContent>
          <span className="sr-only">助手</span>
          <Bubble variant="ghost" align="start" className="w-full">
            <BubbleContent className="w-full">
              <Markdown text={message.text} />
            </BubbleContent>
          </Bubble>
          {message.text ? (
            <MessageFooter>
              <MessageCopyButton text={message.text} onError={onError} />
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
                    scrollAnchor={group.message?.role === "user"}
                  >
                    {group.message ? (
                      <MessageRow message={group.message} onError={onError} />
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
