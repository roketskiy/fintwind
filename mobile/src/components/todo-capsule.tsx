import { useEffect, useRef, useState, type CSSProperties } from "react";
import { BotIcon, CheckIcon, ChevronDownIcon, CloseIcon, FileDiffIcon, ImageIcon, ListIcon, LoaderIcon, TerminalIcon } from "@/components/icons";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import type { BackgroundWorkEntry, SessionDetail, TodoSnapshot } from "@/lib/types";

const statusLabels = {
  pending: "待办",
  in_progress: "执行中",
  completed: "已完成",
  cancelled: "已取消",
};

const tokenFormatter = new Intl.NumberFormat("zh-CN");
const formatTokens = (value: number | null | undefined) => value == null ? "—" : tokenFormatter.format(value);
const formatPercent = (value: number | null) => value === null ? "—" : `${value.toFixed(1)}%`;

function formatBackgroundElapsed(totalSeconds: number) {
  const total = Math.max(0, Math.floor(totalSeconds));
  if (total < 60) return `${total} 秒`;
  if (total < 3600) {
    const minutes = Math.floor(total / 60);
    const seconds = total % 60;
    return seconds > 0 ? `${minutes} 分 ${seconds} 秒` : `${minutes} 分`;
  }
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  return minutes > 0 ? `${hours} 小时 ${minutes} 分` : `${hours} 小时`;
}

// Floats inside the transcript above the composer. The transcript's bottom
// padding creates scrollable clearance, without shrinking its viewport.
export function TodoCapsule({ todos, changes, usage, backgroundWork }: {
  todos?: TodoSnapshot | null;
  changes?: SessionDetail["workspaceChanges"];
  usage?: SessionDetail["usage"];
  backgroundWork?: BackgroundWorkEntry[];
}) {
  const [panel, setPanel] = useState<"todos" | "usage" | "background" | null>(null);
  const [now, setNow] = useState(() => performance.now());
  const trigger = useRef<HTMLButtonElement>(null);
  const usageTrigger = useRef<HTMLButtonElement>(null);
  const backgroundTrigger = useRef<HTMLButtonElement>(null);
  const anchors = useRef(new Map<string, { base: number; at: number }>());
  const liveWork = backgroundWork ?? [];
  const hasTodos = Boolean(todos?.total);
  const hasBackgroundWork = liveWork.length > 0;
  const percent = usage?.contextTokens != null && usage.contextWindow != null && usage.contextWindow > 0
    ? usage.contextTokens * 100 / usage.contextWindow : null;
  const cacheHit = usage?.cacheReadTokens != null && usage.promptTokens != null && usage.promptTokens > 0
    ? Math.min(usage.cacheReadTokens, usage.promptTokens) * 100 / usage.promptTokens : null;
  const usageLabel = formatPercent(percent);
  const open = panel === "usage" || panel === "todos" && hasTodos || panel === "background" && hasBackgroundWork;

  useEffect(() => {
    const recordedAt = performance.now();
    const next = new Map<string, { base: number; at: number }>();
    for (const item of liveWork) {
      const existing = anchors.current.get(item.id);
      next.set(item.id, existing && existing.base === item.elapsedSeconds ? existing : { base: item.elapsedSeconds, at: recordedAt });
    }
    anchors.current = next;
  }, [backgroundWork]);

  useEffect(() => {
    if (panel === "todos" && !hasTodos) setPanel(null);
    if (panel === "background" && !hasBackgroundWork) setPanel(null);
  }, [panel, hasTodos, hasBackgroundWork]);

  useEffect(() => {
    if (panel !== "background" || !hasBackgroundWork) return;
    setNow(performance.now());
    const timer = window.setInterval(() => setNow(performance.now()), 1000);
    return () => window.clearInterval(timer);
  }, [panel, hasBackgroundWork]);

  const currentNow = panel === "background" && hasBackgroundWork ? Math.max(now, performance.now()) : now;

  const label = <span className="todo-capsule-label">
    {changes && <span className="workspace-change-count" role="img" aria-label={`工作区变更：新增 ${changes.additions} 行，删除 ${changes.deletions} 行`}>
      <FileDiffIcon />
      <span className="workspace-additions">+{changes.additions}</span>
      <span className="workspace-deletions">−{changes.deletions}</span>
    </span>}
    {hasTodos && todos && <>
      {changes && <span aria-hidden="true" className="todo-capsule-divider">·</span>}
      <ListIcon /><span>待办</span>
      <span className="todo-count">{todos.completed}/{todos.total}</span>
      <ChevronDownIcon className="todo-chevron" />
    </>}
  </span>;

  return <div className="todo-region" data-open={open || undefined} onKeyDown={(event) => {
    if (event.key !== "Escape" || !open) return;
    event.preventDefault();
    setPanel(null);
    (panel === "usage" ? usageTrigger : panel === "background" ? backgroundTrigger : trigger).current?.focus({ preventScroll: true });
  }}>
    {(hasTodos || changes) && <Collapsible className="todo-capsule-section" open={panel === "todos" && hasTodos} onOpenChange={(value) => setPanel(value ? "todos" : null)}>
    {hasTodos && todos && <CollapsibleContent className="todo-capsule-panel">
      <div className="todo-capsule-list" role="region" aria-label="待办清单" tabIndex={0}>
        <ol>
          {todos.items.map((item, index) => <li key={index} data-status={item.status}>
            <span className="todo-mark" aria-hidden="true">
              {item.status === "completed" ? <CheckIcon /> : item.status === "cancelled" ? <CloseIcon /> : item.status === "in_progress" ? <LoaderIcon /> : <span className="todo-pending-mark" />}
            </span>
            <span className="sr-only">{statusLabels[item.status]}：</span>
            <span className="todo-content">{item.content}</span>
          </li>)}
        </ol>
        {todos.truncated && <p className="todo-truncated">部分待办已省略</p>}
      </div>
    </CollapsibleContent>}
    {hasTodos && todos ? <CollapsibleTrigger ref={trigger} className="todo-capsule-trigger" aria-label={`待办清单，${todos.completed}/${todos.total}${changes ? `；工作区变更：新增 ${changes.additions} 行，删除 ${changes.deletions} 行` : ""}`}>
      {label}
    </CollapsibleTrigger> : <div className="todo-capsule-trigger">{label}</div>}
    </Collapsible>}
    <Collapsible className="usage-capsule-section" open={panel === "usage"} onOpenChange={(value) => setPanel(value ? "usage" : null)}>
      <CollapsibleContent className="todo-capsule-panel">
        <dl className="todo-capsule-list usage-capsule-details" aria-label="会话用量">
          <div><dt>上下文占用</dt><dd>{formatTokens(usage?.contextTokens)} / {formatTokens(usage?.contextWindow)}</dd></div>
          <div><dt>总 token</dt><dd>{formatTokens(usage?.totalTokens)}</dd></div>
          <div><dt>缓存命中率</dt><dd>{formatPercent(cacheHit)}</dd></div>
        </dl>
      </CollapsibleContent>
      <CollapsibleTrigger ref={usageTrigger} className="todo-capsule-trigger usage-capsule-trigger" aria-label={`上下文占用 ${usageLabel}`}>
        <span className="todo-capsule-label" data-tone={percent !== null && percent >= 95 ? "full" : undefined}>
          <span className="usage-capsule-ring" aria-hidden="true" style={{ "--usage-fill": `${Math.min(100, Math.max(0, percent ?? 0))}%` } as CSSProperties} />
          <span className="usage-capsule-percent">{usageLabel}</span>
        </span>
      </CollapsibleTrigger>
    </Collapsible>
    {hasBackgroundWork && <Collapsible className="background-capsule-section" open={panel === "background"} onOpenChange={(value) => setPanel(value ? "background" : null)}>
      <CollapsibleContent className="todo-capsule-panel">
        <div className="todo-capsule-list background-capsule-list" role="region" aria-label="后台任务" tabIndex={0}>
          <ul>
            {liveWork.map((item) => {
              const anchor = anchors.current.get(item.id);
              const elapsed = anchor && anchor.base === item.elapsedSeconds
                ? item.elapsedSeconds + Math.max(0, Math.floor((currentNow - anchor.at) / 1000))
                : item.elapsedSeconds;
              return <li key={item.id} className="background-capsule-row">
                <span className="background-capsule-icon" aria-hidden="true">
                  {item.kind === "subagent" ? <BotIcon /> : item.kind === "image" ? <ImageIcon /> : <TerminalIcon />}
                </span>
                <span className="background-capsule-name">{item.title}</span>
                <span className="background-capsule-elapsed">{formatBackgroundElapsed(elapsed)}</span>
              </li>;
            })}
          </ul>
        </div>
      </CollapsibleContent>
      <CollapsibleTrigger ref={backgroundTrigger} className="todo-capsule-trigger background-capsule-trigger" aria-label={`后台任务，${liveWork.length} 项`}>
        <span className="todo-capsule-label">
          <BotIcon />
          <span>后台</span>
          <span className="todo-count">{liveWork.length}</span>
        </span>
      </CollapsibleTrigger>
    </Collapsible>}
  </div>;
}
