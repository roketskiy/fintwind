import * as React from "react";

import {
  DesktopRequiredAlert,
  PermissionCard,
  QuestionsCard,
} from "@/components/approvals";
import { CheckIcon, CopyIcon, MessageIcon } from "@/components/icons";
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
import type { RemoteMessage, SessionDetail } from "@/lib/types";

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
        <Message align="end">
          <MessageContent>
            <span className="sr-only">你</span>
            <Bubble variant="secondary" align="end">
              <BubbleContent>
                <div className="user-message-text whitespace-pre-wrap">
                  {message.text}
                </div>
              </BubbleContent>
            </Bubble>
          </MessageContent>
        </Message>
      );
    }

    return (
      <Message align="start">
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
  (previous, next) =>
    previous.message.id === next.message.id &&
    previous.message.role === next.message.role &&
    previous.message.text === next.message.text,
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
    session.messages.length === 0 &&
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

              {session.messages.map((message) => (
                <MessageScrollerItem
                  key={message.id}
                  messageId={message.id}
                  scrollAnchor={message.role === "user"}
                >
                  <MessageRow message={message} onError={onError} />
                </MessageScrollerItem>
              ))}

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
