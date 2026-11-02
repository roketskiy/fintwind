// Adapted from Beautiful UI by Shane Levine (MIT), revision 44a274e598395ab61e7c96c26fda2758780253b7.
// Source: components/primitives/ThinkingState.tsx, Reasoning variant — expandable
// header (status + chevron), vertical rail, prose content, settle-once behavior.
// Demo stage timers (STAGES/useSequence), synthetic rows and staged follow-up
// sequencing are intentionally excluded: expanded/working/settled derive from the
// real RemoteActivity (reasoning text, complete, failed). Tool rows show name +
// status only and never render tool body text or target.
import * as React from "react";

import { ChevronDownIcon, LoaderIcon, SearchIcon, SparkleIcon } from "@/components/icons";
import { CollapsibleTrigger } from "@/components/ui/collapsible";
import {
  TraceCollapsible,
  TraceCollapsibleContent,
  TraceDescription,
  TraceIcon,
  TraceLine,
  TraceTitle,
} from "@/components/ui/trace";
import type { RemoteActivity } from "@/lib/types";

function statusOf(activity: RemoteActivity) {
  if (activity.failed) {
    return { status: "error" as const, tone: "error", text: "失败" };
  }
  if (!activity.complete) {
    return { status: "running" as const, tone: "running", text: "进行中" };
  }
  return { status: undefined, tone: "done", text: "完成" };
}

export function ReasoningActivityRow({
  activity,
}: {
  activity: RemoteActivity;
}) {
  const { status } = statusOf(activity);
  const working = !activity.complete && !activity.failed;
  const [manualExpanded, setManualExpanded] = React.useState<boolean | null>(
    null,
  );
  // Natural settle: open while running, settled (closed) once done, unless the
  // user explicitly toggled.
  const expanded = manualExpanded ?? working;

  const label = activity.failed ? "思考失败" : working ? "正在思考" : "思考完成";
  const heading = <><SparkleIcon /><span role="status" className={working ? "bui-reasoning-shimmer" : undefined}>{label}</span></>;

  if (!activity.text) {
    return <div className="bui-thinking-trigger" data-status={status}>{heading}</div>;
  }

  return (
    <div className="bui-think">
      <TraceCollapsible open={expanded} onOpenChange={setManualExpanded}>
        <CollapsibleTrigger data-slot="trace-collapsible-trigger" className="bui-thinking-trigger bui-press" data-status={status}>
          {heading}<ChevronDownIcon className="bui-thinking-chevron" />
        </CollapsibleTrigger>
        <TraceCollapsibleContent>
          {activity.truncated && <p>较早的思考已省略</p>}
          <div className="reasoning-text">{activity.text}</div>
        </TraceCollapsibleContent>
      </TraceCollapsible>
    </div>
  );
}

export function ToolActivityRow({ activity }: { activity: RemoteActivity }) {
  const { status, tone, text } = statusOf(activity);

  return (
    <TraceLine status={status} className="bui-tool">
      <TraceIcon>{activity.complete ? <SearchIcon /> : <LoaderIcon />}</TraceIcon>
      <TraceTitle>{activity.title || "工具调用"}</TraceTitle>
      <TraceDescription className="bui-tool-status" data-tone={tone}>
        {text}
      </TraceDescription>
    </TraceLine>
  );
}
