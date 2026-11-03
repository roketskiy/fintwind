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
import { TodoCapsule } from "@/components/todo-capsule";
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

const ActivityBranch = React.memo(function ActivityBranch({ activities, turnActive }: { activities: RemoteActivity[]; turnActive: boolean }) {
  const failures = activities.filter((activity) => activity.failed).length;
  const [manualExpanded, setManualExpanded] = React.useState<boolean | null>(null);
  const { end: readingAboveLatest } = useMessageScrollerScrollable();
  const [autoExpanded, setAutoExpanded] = React.useState(turnActive);
  const [showAll, setShowAll] = React.useState(false);
  const [range, setRange] = React.useState<{ first: string; last: string } | null>(null);
  const tailStart = Math.max(0, activities.length - ACTIVITY_PAGE_SIZE);
  const pinnedEnd = range ? activities.findIndex((activity) => activity.id === range.last) : -1;
  const end = !showAll && pinnedEnd >= 0 ? pinnedEnd + 1 : activities.length;
  const start = showAll ? 0 : pinnedEnd >= 0 && range
    ? Math.max(0, activities.findIndex((activity) => activity.id === range.first)) : tailStart;
  React.useLayoutEffect(() => {
    if (showAll) return;
    if (range && pinnedEnd < 0) setRange(null);
    else if (!readingAboveLatest && range) setRange(null);
    else if (readingAboveLatest && !range && activities[tailStart] && activities.at(-1)) {
      setRange({ first: activities[tailStart]!.id, last: activities.at(-1)!.id });
    }
  }, [activities, readingAboveLatest, range, pinnedEnd, tailStart, showAll]);
  React.useEffect(() => {
    if (turnActive) setAutoExpanded(true);
    else if (!readingAboveLatest) setAutoExpanded(false);
  }, [turnActive, readingAboveLatest]);
  // Do not remove the activity a reader is inspecting when it completes.
  // The next assistant reply closes this activity segment. A completed tool
  // alone does not. Preserve reading position and explicit disclosure choices.
  const expanded = manualExpanded ?? (turnActive || (autoExpanded && readingAboveLatest));
  return <Collapsible className="bui-branch" data-active-segment={turnActive} data-manual-disclosure={manualExpanded === null ? undefined : manualExpanded ? "open" : "closed"} open={expanded} onOpenChange={setManualExpanded}>
    <div className="bui-branch-header">
      <CollapsibleTrigger className="bui-branch-trigger">
        <ChevronRightIcon className="bui-branch-chevron" />
        <span>{activities.length} 项活动</span>
        {failures > 0 && <span className="bui-branch-status" data-failed>{failures} 项失败</span>}
      </CollapsibleTrigger>
      {expanded && <div className="bui-branch-actions">
        {start > 0 && <Button type="button" variant="ghost" size="sm" className="bui-branch-range" aria-label={`展开全部 ${activities.length} 项活动`} onClick={() => setShowAll(true)}>全部展开</Button>}
        {(showAll || end < activities.length) && <Button type="button" variant="ghost" size="sm" className="bui-branch-range" aria-label={`只显示最新 ${ACTIVITY_PAGE_SIZE} 项活动`} onClick={() => { setShowAll(false); setRange(null); }}>最新</Button>}
      </div>}
    </div>
    <CollapsibleContent>
      <div className="bui-branch-panel">
      <Trace className="bui-branch-list" role="list">
        {activities.slice(start, end).map((activity) => <div className="bui-branch-item" role="listitem" data-kind={activity.kind} key={activity.id}>
          <ActivityRow activity={activity} />
        </div>)}
      </Trace>
      </div>
    </CollapsibleContent>
  </Collapsible>;
}, (previous, next) => previous.turnActive === next.turnActive && previous.activities.length === next.activities.length && previous.activities.every((activity, index) => activity === next.activities[index]));

interface TranscriptGroup {
  id: string;
  message?: RemoteMessage;
  activities?: RemoteActivity[];
}

function transcriptGroups(entries: TranscriptEntry[]) {
  const groups: TranscriptGroup[] = [];
  for (const entry of entries) {
    if (entry.type !== "activity")
      groups.push({ id: `message:${entry.id}`, message: entry });
    else {
      const previous = groups.at(-1);
      if (previous?.activities && previous.activities[0]?.turnId === entry.turnId) previous.activities.push(entry);
      else groups.push({ id: previous ? `activity:after:${previous.id}` : "activity:start", activities: [entry] });
    }
  }
  return groups;
}

function transcriptTurns(groups: TranscriptGroup[]) {
  const turns: { id: string; turnId?: string; groups: { group: TranscriptGroup; index: number }[] }[] = [];
  groups.forEach((group, index) => {
    const turnId = (group.message ?? group.activities?.[0])?.turnId;
    const previous = turns.at(-1);
    // Provider turn IDs keep steering prompts in their original turn. Older
    // desktops can only delimit turns by the next user message.
    if (previous && (turnId ? previous.turnId === turnId : !previous.turnId && group.message?.role !== "user")) {
      previous.groups.push({ group, index });
    } else {
      turns.push({ id: `turn:${turnId ?? group.id}`, turnId, groups: [{ group, index }] });
    }
  });
  return turns;
}

function TurnTranscript({ turn, settled, renderGroup }: {
  turn: ReturnType<typeof transcriptTurns>[number];
  settled: boolean;
  renderGroup: (group: TranscriptGroup, index: number, nested?: boolean) => React.ReactNode;
}) {
  const { end: readingAboveLatest } = useMessageScrollerScrollable();
  const [deferCollapse, setDeferCollapse] = React.useState(!settled);
  const [expanded, setExpanded] = React.useState(false);
  React.useEffect(() => {
    if (!settled) setDeferCollapse(true);
    else if (!readingAboveLatest) setDeferCollapse(false);
  }, [settled, readingAboveLatest]);
  const preceding = turn.groups.slice(0, -1);
  // The opening user message is never process content. Later user messages
  // with the same provider turn ID are steering and may share the disclosure.
  const prompt = preceding[0]?.group.message?.role === "user" ? preceding[0] : undefined;
  const history = prompt ? preceding.slice(1) : preceding;
  const answer = turn.groups.at(-1)!;
  const disclose = settled && history.length > 0 && !(deferCollapse && readingAboveLatest);
  return <>
    {prompt && renderGroup(prompt.group, prompt.index)}
    {disclose ? (
      <MessageScrollerItem key={turn.id} messageId={turn.id} data-transcript-kind="turn-history">
        <Collapsible className="bui-turn-history" open={expanded} onOpenChange={setExpanded}>
          <CollapsibleTrigger className="bui-branch-trigger">
            <ChevronRightIcon className="bui-branch-chevron" />
            <span>本轮过程</span>
          </CollapsibleTrigger>
          <CollapsibleContent>
            <div className="bui-turn-history-content">
              {history.map(({ group, index }) => renderGroup(group, index, true))}
            </div>
          </CollapsibleContent>
        </Collapsible>
      </MessageScrollerItem>
    ) : history.map(({ group, index }) => renderGroup(group, index))}
    {renderGroup(answer.group, answer.index)}
  </>;
}

export interface ConversationProps {
  session: SessionDetail;
  disabled: boolean;
  busy: boolean;
  onPermission: (optionId: string) => void;
  onAnswers: (answers: { questionId: string; answers: string[] }[]) => Promise<boolean> | void;
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
    responseComplete,
    onError,
  }: {
    message: RemoteMessage;
    responseComplete: boolean;
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

    const showFooter = responseComplete && Boolean(message.text || clock || elapsed);
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
      previous.responseComplete === next.responseComplete &&
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
  const groups = transcriptGroups(session.entries ?? session.messages);
  // Old desktops have no turn IDs. Keep their conservative fallback, but use
  // desktop-owned turn state and final-part flags whenever they are available.
  let currentTurnStart = -1;
  for (let index = groups.length - 1; index >= 0; index--) {
    if (groups[index]?.message?.role === "user") { currentTurnStart = index; break; }
  }
  const turnActive = ["working", "connecting", "waiting"].includes(session.status);
  const lastReplyByTurn = new Map<string, number>();
  let lastReply = -1;
  groups.forEach((group, index) => {
    if (group.message?.role !== "assistant") return;
    lastReply = index;
    if (group.message.turnId) lastReplyByTurn.set(group.message.turnId, index);
  });
  // Only a settled trailing assistant part owns a response footer. Tool rows
  // invalidate preceding commentary; another prompt seals the preceding turn.
  // Do not infer completion from a timestamp or a temporarily finished tool.
  const responseFooters = new Set<string>();
  let finalPart: RemoteMessage | undefined;
  for (const group of groups) {
    if (group.message?.role === "user") {
      if (finalPart && !finalPart.streaming) responseFooters.add(finalPart.id);
      finalPart = undefined;
    } else if (group.message) finalPart = group.message;
    else finalPart = undefined;
  }
  if (!turnActive && finalPart && !finalPart.streaming) responseFooters.add(finalPart.id);
  for (const group of groups) {
    const message = group.message;
    if (message?.responseComplete !== undefined) {
      responseFooters.delete(message.id);
      if (message.responseComplete && !message.streaming) responseFooters.add(message.id);
    }
  }
  const turns = transcriptTurns(groups);
  const renderGroup = (group: TranscriptGroup, index: number, nested = false) => {
    const message = group.message;
    const activity = group.activities?.[0];
    const segmentRunning = activity?.turnRunning ?? (turnActive && index > currentTurnStart);
    const lastSegmentReply = activity?.turnId ? lastReplyByTurn.get(activity.turnId) ?? -1 : lastReply;
    const intermediate = Boolean(message?.role === "assistant" && !responseFooters.has(message.id) && index < groups.length - 1);
    const kind = group.activities ? "activity" : message?.role === "user" ? "prompt" : intermediate ? "progress" : "answer";
    const content = message ? <MessageRow message={message} responseComplete={responseFooters.has(message.id)} onError={onError} />
      : <ActivityBranch activities={group.activities ?? []} turnActive={segmentRunning && index > lastSegmentReply} />;
    // A closed turn registers one measured scroller item, never nested items.
    return nested ? <div key={group.id} data-message-id={group.id} data-transcript-kind={kind}>{content}</div>
      : <MessageScrollerItem key={group.id} messageId={group.id} data-transcript-kind={kind}>{content}</MessageScrollerItem>;
  };
  const pending = session.permission !== null || session.questions !== null;
  const desktopBlocked = session.desktopRequired;
  const empty =
    (session.entries ?? session.messages).length === 0 &&
    !session.todos?.total &&
    !session.workspaceChanges &&
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
        <TodoCapsule key={session.id} todos={session.todos} changes={session.workspaceChanges} usage={session.usage} />
      </div>
    );
  }

  return (
    <div className="conversation-body flex min-h-0 min-w-0 flex-1 flex-col" data-capsule-clearance>
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

              {turns.map((turn) => {
                const last = turn.groups.at(-1)!;
                const message = last.group.message;
                const settled = Boolean(message?.role === "assistant" && responseFooters.has(message.id)
                  && !turn.groups.some(({ group }) => (group.message ?? group.activities?.[0])?.turnRunning === true)
                  && (message.turnRunning === false || !turnActive || last.index < currentTurnStart));
                return <TurnTranscript key={`${session.id}:${turn.id}`} turn={turn} settled={settled} renderGroup={renderGroup} />;
              })}

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
      <TodoCapsule key={session.id} todos={session.todos} changes={session.workspaceChanges} usage={session.usage} />
    </div>
  );
}
