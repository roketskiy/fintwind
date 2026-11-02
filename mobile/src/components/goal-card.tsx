// TaskRows structure adapted from Beautiful UI (MIT). Only desktop-reported state is shown.
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { CheckIcon, ChevronDownIcon, PauseIcon, PlayIcon, TargetIcon, TrashIcon } from "@/components/icons";
import { PixelLoader } from "@/components/beautiful/primitives";
import type { SessionDetail } from "@/lib/types";

const labels = { active: "进行中", paused: "已暂停", blocked: "受阻", done: "已完成" };

export function GoalCard({ goal, disabled, onControl }: {
  goal: NonNullable<SessionDetail["goal"]>;
  disabled: boolean;
  onControl: (action: "pause" | "resume" | "clear") => void;
}) {
  const minutes = Math.floor((goal.elapsedSeconds ?? 0) / 60);
  return <Collapsible className="goal-card">
    <div className="goal-heading">
      <CollapsibleTrigger render={<Button variant="ghost" className="goal-trigger" />}>
        <span className="goal-symbol">{goal.status === "active" ? <PixelLoader compact label="目标进行中" /> : goal.status === "done" ? <CheckIcon /> : <TargetIcon />}</span>
        <span className="goal-summary"><strong>Goal</strong><span>{goal.objective}</span></span><Badge variant={goal.status === "blocked" ? "destructive" : "secondary"}>{labels[goal.status]}</Badge>
        <ChevronDownIcon data-icon="inline-end" />
      </CollapsibleTrigger>
      <div className="goal-controls">
        {goal.status === "active" && <Button variant="ghost" size="icon" disabled={disabled} aria-label="暂停目标" onClick={() => onControl("pause")}><PauseIcon /></Button>}
        {["paused", "blocked"].includes(goal.status) && <Button variant="ghost" size="icon" disabled={disabled} aria-label="继续目标" onClick={() => onControl("resume")}><PlayIcon /></Button>}
        <Button variant="ghost" size="icon" disabled={disabled} aria-label="清除目标" onClick={() => onControl("clear")}><TrashIcon /></Button>
      </div>
    </div>
    <CollapsibleContent className="goal-body">
      <p className="goal-objective">{goal.objective}</p>
      {goal.reason && <p className="goal-reason">{goal.reason}</p>}
      {goal.result && <p className="goal-result">{goal.result}</p>}
      <div className="goal-meta">
        <span>{goal.maxTurns > 0 ? `${goal.turns} / ${goal.maxTurns} 轮` : `${goal.turns} 轮`}</span>
        {goal.tools !== undefined && <span>{goal.tools} 次工具调用</span>}
        {goal.elapsedSeconds !== undefined && <span>{minutes > 0 ? `${minutes} 分 ${Math.floor(goal.elapsedSeconds % 60)} 秒` : `${Math.floor(goal.elapsedSeconds)} 秒`}</span>}
      </div>
    </CollapsibleContent>
  </Collapsible>;
}
