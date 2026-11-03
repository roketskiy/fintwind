import { useRef, useState, type CSSProperties } from "react";
import { CheckIcon, ChevronDownIcon, CloseIcon, FileDiffIcon, ListIcon, LoaderIcon } from "@/components/icons";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import type { SessionDetail, TodoSnapshot } from "@/lib/types";

const statusLabels = {
  pending: "待办",
  in_progress: "执行中",
  completed: "已完成",
  cancelled: "已取消",
};

const tokenFormatter = new Intl.NumberFormat("zh-CN");
const formatTokens = (value: number | null | undefined) => value == null ? "—" : tokenFormatter.format(value);
const formatPercent = (value: number | null) => value === null ? "—" : `${value.toFixed(1)}%`;

// Floats inside the transcript above the composer. The transcript's bottom
// padding creates scrollable clearance, without shrinking its viewport.
export function TodoCapsule({ todos, changes, usage }: { todos?: TodoSnapshot | null; changes?: SessionDetail["workspaceChanges"]; usage?: SessionDetail["usage"] }) {
  const [panel, setPanel] = useState<"todos" | "usage" | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const usageTrigger = useRef<HTMLButtonElement>(null);
  const hasTodos = Boolean(todos?.total);
  const percent = usage?.contextTokens != null && usage.contextWindow != null && usage.contextWindow > 0
    ? usage.contextTokens * 100 / usage.contextWindow : null;
  const cacheHit = usage?.cacheReadTokens != null && usage.promptTokens != null && usage.promptTokens > 0
    ? Math.min(usage.cacheReadTokens, usage.promptTokens) * 100 / usage.promptTokens : null;
  const usageLabel = formatPercent(percent);
  const open = panel === "usage" || panel === "todos" && hasTodos;

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
    (panel === "usage" ? usageTrigger : trigger).current?.focus({ preventScroll: true });
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
  </div>;
}
