// Isolated visual fixtures. This entry never calls useRemote/start and is not a production input.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { RemoteView } from "@/app";
import type { useRemote } from "@/hooks/use-remote";
import { applyTheme } from "@/lib/theme";
import type { CommandMap, ComposerOptions, SessionDetail } from "@/lib/types";
import "@/index.css";

const params = new URLSearchParams(location.search);
applyTheme(params.get("theme") === "dark" ? "dark" : "light");
localStorage.setItem("fintwind-phone-theme", params.get("theme") === "dark" ? "dark" : "light");
const options: ComposerOptions = {
  canConfigure: true,
  models: [
    { id: "preview/model", name: "GPT-6.1 Terra", provider: "预览提供商", efforts: [{ id: "low", label: "low" }, { id: "medium", label: "medium" }, { id: "high", label: "high" }, { id: "max", label: "max" }] },
    { id: "preview/muse", name: "Muse Spark", provider: "预览提供商", efforts: [] },
  ],
  commands: [{ name: "compact", description: "压缩会话上下文" }, { name: "review", description: "审查当前改动" }],
};
const detail: SessionDetail = {
  id: "visual-session", projectId: "visual-project", title: "调整移动端的消息与输入体验", status: "idle",
  features: ["orderedTranscript", "attachments", "composerOptions", "goals"], model: "preview/model", modelName: "GPT-6.1 Terra", interactionMode: "build", reasoningEffort: "max",
  canSend: true, desktopRequired: false, truncated: false, permission: null, questions: null,
  goal: { objective: "优化手机上的对话体验，保留完整的模型选项与审批流程。", status: "active", turns: 3, maxTurns: 12, reason: "", result: "", tools: 8, elapsedSeconds: 149 },
  messages: [],
  entries: [
    { id: "u1", role: "user", text: "让输入框更简洁，命令和模型选择都能方便地使用。", createdAt: 1793635200 },
    { id: "r1", type: "activity", kind: "reasoning", title: "", target: "", text: "先保留真实业务状态，再统一输入框、菜单和目标卡片的层级。日间与夜间使用相同的语义配色。", complete: true, failed: false, truncated: false },
    { id: "t1", type: "activity", kind: "tool", title: "读取组件", target: "", text: "", complete: true, failed: false, truncated: false },
    { id: "t2", type: "activity", kind: "tool", title: "更新样式", target: "", text: "", complete: true, failed: false, truncated: false },
    { id: "a1", role: "assistant", createdAt: 1793635349, elapsedSeconds: 149, text: params.get("scene") === "code"
      ? ['```ts', '// Keep markup as text.', 'const greet = (name: string) => `Hello, ${name}`;', 'console.log(greet("<img src=x onerror=alert(1)>"));', '```', '', '```json', '{ "enabled": true, "count": 42, "name": "Fintwind" }', '```', '', '```fintwind-unknown', '<script>not executable</script>', 'plain & unchanged', '```'].join('\n')
      : "已整理为两个清晰的层级：\n\n- **输入区域**：消息、附件与发送。\n- **会话选项**：模型、模式和思考强度。\n\n输入 `/` 可以筛选命令。Goal 显示真实状态和轮次，不使用虚构的完成百分比。\n\n```css\n.prompt-card {\n  border-radius: 14px;\n  background: var(--card);\n}\n```" },
  ],
};

function Preview() {
  const [page, setPage] = useState<"sessions" | "conversation">(params.get("scene") === "list" ? "sessions" : "conversation");
  const [goal, setGoal] = useState(detail.goal);
  const [selection, setSelection] = useState({ model: detail.model, interactionMode: detail.interactionMode, reasoningEffort: detail.reasoningEffort });
  const approval = params.get("scene") === "approval" || params.get("scene") === "multi";
  const showing = { ...detail, ...selection, modelName: options.models.find((item) => item.id === selection.model)?.name ?? selection.model,
    entries: params.get("busy") === "1" ? detail.entries?.map((entry) => entry.id === "t2" && entry.type === "activity" ? { ...entry, complete: false } : entry) : detail.entries,
    goal: approval ? null : goal, status: approval ? "waiting" as const : params.get("busy") === "1" ? "working" as const : detail.status,
    ...(approval ? { questions: { requestId: "visual-question", questions: params.get("scene") === "approval" ? [
    { id: "q1", header: "确认", question: "请在安装版 Fintwind 中重新选中当前聊天并保持该聊天打开。确认后我会再次尝试访问公开网页。", multiSelect: false, options: [
      { label: "已选中，继续", description: "当前聊天已重新选中，可以继续。" },
      { label: "一直处于当前聊天", description: "始终选中本聊天，没有切换过。" },
      { label: "停止", description: "保留已记录的结果，不再尝试。" },
    ] },
    ] : [
    { id: "q1", header: "配色", question: "页面默认采用哪种配色？", multiSelect: false, options: [{ label: "跟随系统", description: "根据设备外观切换" }, { label: "日间", description: "" }, { label: "夜间", description: "" }] },
    { id: "q2", header: "输入体验", question: "需要保留哪些快捷入口？", multiSelect: true, options: [{ label: "斜杠命令", description: "" }, { label: "模型选择", description: "" }, { label: "附件", description: "" }] },
   ] } } : {}) };
  const app: ReturnType<typeof useRemote> = {
    connection: "connected", error: null, pendingScopes: [], projectId: detail.projectId, sessionId: detail.id, page,
    workspaces: [{ id: detail.projectId, name: "Fintwind" }], sessions: [
      { id: detail.id, projectId: detail.projectId, title: detail.title, status: "working", updatedAt: Date.now() / 1000 },
      { id: "visual-2", projectId: detail.projectId, title: "统一日间与夜间的界面层级", status: "idle", updatedAt: Date.now() / 1000 - 3600 },
      { id: "visual-3", projectId: detail.projectId, title: "检查远控审批与目标状态", status: "waiting", updatedAt: Date.now() / 1000 - 86400 },
    ], detail: showing, loading: false, busy: false,
    selectWorkspace: () => {}, openSession: () => setPage("conversation"), back: () => setPage("sessions"), createSession: async () => {}, setError: () => {}, refresh: () => {}, reconnect: () => {}, forget: async () => {},
    mutate: async (type, fields) => {
      if (type === "configureSession") {
        setSelection((current) => ({ ...current,
          ...("model" in fields && typeof fields.model === "string" ? { model: fields.model } : {}),
          ...("interactionMode" in fields && typeof fields.interactionMode === "string" ? { interactionMode: fields.interactionMode } : {}),
          ...("reasoningEffort" in fields && typeof fields.reasoningEffort === "string" ? { reasoningEffort: fields.reasoningEffort } : {}),
        }));
        return { accepted: true } as CommandMap[typeof type]["result"];
      }
      if ("text" in fields && fields.text === "/goal clear") setGoal(null);
      return null;
    },
  };
  return <RemoteView app={app} previewOptions={options} />;
}
createRoot(document.getElementById("root")!).render(<Preview />);
