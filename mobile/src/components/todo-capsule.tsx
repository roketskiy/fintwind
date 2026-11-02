import { useRef, useState } from "react";
import { CheckIcon, ChevronDownIcon, CloseIcon, FileDiffIcon, ListIcon, LoaderIcon } from "@/components/icons";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import type { SessionDetail, TodoSnapshot } from "@/lib/types";

const statusLabels = {
  pending: "待办",
  in_progress: "执行中",
  completed: "已完成",
  cancelled: "已取消",
};

// Floats inside the transcript above the composer. The transcript's bottom
// padding creates scrollable clearance, without shrinking its viewport.
export function TodoCapsule({ todos, changes }: { todos?: TodoSnapshot | null; changes?: SessionDetail["workspaceChanges"] }) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const hasTodos = Boolean(todos?.total);
  if (!hasTodos && !changes) return null;

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

  return <Collapsible className="todo-region" open={open && hasTodos} onOpenChange={setOpen} onKeyDown={(event) => {
    if (event.key !== "Escape" || !open) return;
    event.preventDefault();
    setOpen(false);
    trigger.current?.focus({ preventScroll: true });
  }}>
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
  </Collapsible>;
}
