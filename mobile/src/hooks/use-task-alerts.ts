import { useEffect, useRef, useState } from "react";
import { remoteClient } from "@/lib/remote-client";
import type { ClientSnapshot, TaskActivity } from "@/lib/types";

export interface TaskAlert { id: string; sessionId: string; projectId: string; title: string; label: string }

export function useTaskAlerts(connection: ClientSnapshot["connection"], viewing: { sessionId: string; projectId: string } | null) {
  const [alerts, setAlerts] = useState<TaskAlert[]>([]);
  const previous = useRef<Map<string, TaskActivity>>(new Map());
  const seen = useRef(new Set<string>());
  const initialized = useRef(false);
  const started = useRef(Math.floor(Date.now() / 1000));
  const currentView = useRef(viewing);
  currentView.current = viewing;
  // Opening a task acknowledges its queued alerts; leaving it must not make
  // events that were already visible in that conversation reappear.
  useEffect(() => {
    if (viewing) setAlerts(current => current.filter(alert => alert.sessionId !== viewing.sessionId || alert.projectId !== viewing.projectId));
  }, [viewing?.sessionId, viewing?.projectId]);
  useEffect(() => {
    if (connection === "unpaired") {
      previous.current.clear(); seen.current.clear(); initialized.current = false; setAlerts([]); started.current = Math.floor(Date.now() / 1000);
    }
    if (connection !== "connected") return;
    let active = true, fetching = false, timer = 0;
    let supported: boolean | null = null;
    const receive = (tasks: TaskActivity[]) => {
      const added: TaskAlert[] = [];
      for (const task of tasks) {
        const old = previous.current.get(`${task.projectId}:${task.id}`);
        const add = (event: string, label: string) => {
          const id = `${task.projectId}:${task.id}:${event}`;
          if (seen.current.has(id)) return;
          seen.current.add(id);
          if (currentView.current?.sessionId === task.id && currentView.current.projectId === task.projectId) return;
          added.push({ id, sessionId: task.id, projectId: task.projectId, title: task.title || "未命名会话", label });
        };
        if (initialized.current && task.completed && task.completed.id !== old?.completed?.id && (old?.completed || task.completed.at >= started.current)) {
          if (task.completed.status === "completed") add(`done:${task.completed.id}`, "任务已完成");
          if (task.completed.status === "failed") add(`failed:${task.completed.id}`, "任务失败");
        }
        if (task.permissionId && task.permissionId !== old?.permissionId) add(`permission:${task.permissionId}`, "需要审批");
        if (task.questionId && task.questionId !== old?.questionId) add(`question:${task.questionId}`, "需要回答");
      }
      previous.current = new Map(tasks.map(task => [`${task.projectId}:${task.id}`, task]));
      initialized.current = true;
      while (seen.current.size > 512) seen.current.delete(seen.current.values().next().value!);
      if (added.length) setAlerts(current => [...added.reverse(), ...current].slice(0, 8));
    };
    const refresh = async () => {
      if (!active || fetching || document.hidden || supported === false) return;
      fetching = true;
      window.clearTimeout(timer);
      try {
        if (supported === null) {
          const catalog = await remoteClient.command("listWorkspaces", {});
          if (!active) return;
          supported = catalog.features?.includes("taskActivity") === true;
          if (!supported) return;
        }
        const result = await remoteClient.command("getTaskActivity", {});
        if (active) receive(result.tasks);
      } catch { /* Reconnect/read errors have their own surface. */ }
      finally {
        fetching = false;
        if (active && !document.hidden && supported !== false) timer = window.setTimeout(() => void refresh(), 2000);
      }
    };
    const visibility = () => { window.clearTimeout(timer); if (!document.hidden) void refresh(); };
    document.addEventListener("visibilitychange", visibility);
    void refresh();
    return () => { active = false; window.clearTimeout(timer); document.removeEventListener("visibilitychange", visibility); };
  }, [connection]);
  return { alerts, dismiss: (id: string) => setAlerts(current => current.filter(alert => alert.id !== id)) };
}
