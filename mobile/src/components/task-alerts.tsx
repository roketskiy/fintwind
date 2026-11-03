import { Button } from "@/components/ui/button";
import { CheckIcon, CloseIcon, InfoIcon, LockIcon, MessageIcon } from "@/components/icons";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "@/components/ui/popover";
import { useTaskAlerts, type TaskAlert } from "@/hooks/use-task-alerts";
import type { ClientSnapshot } from "@/lib/types";

function alertAppearance(label: string) {
  switch (label) {
    case "任务已完成": return { Icon: CheckIcon, text: "已完成" };
    case "任务失败": return { Icon: InfoIcon, text: "失败" };
    case "需要审批": return { Icon: LockIcon, text: "待审批" };
    default: return { Icon: MessageIcon, text: "待回答" };
  }
}

function AlertLabel({ alert }: { alert: TaskAlert }) {
  const { Icon, text } = alertAppearance(alert.label);
  return <><Icon data-icon="inline-start" /><span className="task-alert-title">{alert.title}</span><span className="task-alert-status">{text}</span></>;
}

export function TaskAlerts({ connection, viewing, onOpen }: {
  connection: ClientSnapshot["connection"];
  viewing: { sessionId: string; projectId: string } | null;
  onOpen: (sessionId: string, projectId: string) => void;
}) {
  const { alerts, dismiss } = useTaskAlerts(connection, viewing);
  const visible = alerts.filter(alert => alert.sessionId !== viewing?.sessionId || alert.projectId !== viewing?.projectId);
  const latest = visible[0];
  const open = (alert: TaskAlert) => { onOpen(alert.sessionId, alert.projectId); dismiss(alert.id); };
  return <aside className="task-alerts" aria-label="任务提醒" aria-live="polite">
    {latest && <div className="task-alert-bar" data-task-alert={latest.id}>
      <Button type="button" variant="ghost" className="task-alert-open" aria-label={`${latest.label}：${latest.title}，查看会话`} onClick={() => open(latest)}><AlertLabel alert={latest} /></Button>
      {visible.length > 1 && <Popover>
        <PopoverTrigger render={<Button type="button" variant="ghost" size="icon" aria-label={`查看全部 ${visible.length} 条提醒`} />}>+{visible.length - 1}</PopoverTrigger>
        <PopoverContent align="end" className="task-alert-menu"><PopoverTitle className="sr-only">任务提醒</PopoverTitle>
          {visible.map(alert => <Button type="button" variant="row" key={alert.id} className="task-alert-open" aria-label={`${alert.label}：${alert.title}，查看会话`} onClick={() => open(alert)}><AlertLabel alert={alert} /></Button>)}
        </PopoverContent>
      </Popover>}
      <Button type="button" variant="ghost" size="icon" aria-label={`关闭提醒：${latest.title}`} onClick={() => dismiss(latest.id)}><CloseIcon /></Button>
    </div>}
  </aside>;
}
