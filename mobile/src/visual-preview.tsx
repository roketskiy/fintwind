// Isolated visual fixtures. This entry never calls useRemote/start and is not a production input.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { RemoteView } from "@/app";
import type { useRemote } from "@/hooks/use-remote";
import { applyTheme } from "@/lib/theme";
import type { ComposerOptions, SessionDetail } from "@/lib/types";
import "@/index.css";

const params = new URLSearchParams(location.search);
applyTheme(params.get("theme") === "dark" ? "dark" : "light");
localStorage.setItem("fintwind-phone-theme", params.get("theme") === "dark" ? "dark" : "light");
const options: ComposerOptions = {
  canConfigure: true,
  models: [
    { id: "preview/model", name: "GPT-6.1 Sol", provider: "预览提供商", efforts: [{ id: "medium", label: "中" }, { id: "high", label: "高" }] },
    { id: "preview/muse", name: "Muse Spark", provider: "预览提供商", efforts: [] },
  ],
  commands: [{ name: "compact", description: "压缩会话上下文" }, { name: "review", description: "审查当前改动" }],
};
const detail: SessionDetail = {
  id: "visual-session", projectId: "visual-project", title: "调整移动端的消息与输入体验", status: "idle",
  features: ["orderedTranscript", "attachments", "composerOptions", "goals"], model: "preview/model", modelName: "GPT-6.1 Sol", interactionMode: "build", reasoningEffort: "high",
  canSend: true, desktopRequired: false, truncated: false, permission: null, questions: null,
  goal: { objective: "优化手机上的对话体验，保留完整的模型选项与审批流程。", status: "active", turns: 3, maxTurns: 12, reason: "", result: "", tools: 8, elapsedSeconds: 149 },
  messages: [],
  entries: [
    { id: "u1", role: "user", text: "让输入框更简洁，命令和模型选择都能方便地使用。", createdAt: 1793635200 },
    { id: "r1", type: "activity", kind: "reasoning", title: "", target: "", text: "先保留真实业务状态，再统一输入框、菜单和目标卡片的层级。日间与夜间使用相同的语义配色。", complete: true, failed: false, truncated: false },
    { id: "t1", type: "activity", kind: "tool", title: "读取组件", target: "", text: "", complete: true, failed: false, truncated: false },
    { id: "t2", type: "activity", kind: "tool", title: "更新样式", target: "", text: "", complete: true, failed: false, truncated: false },
    { id: "a1", role: "assistant", createdAt: 1793635349, elapsedSeconds: 149, text: "已整理为两个清晰的层级：\n\n- **输入区域**：消息、附件与发送。\n- **会话选项**：模型、模式和思考强度。\n\n输入 `/` 可以筛选命令。Goal 显示真实状态和轮次，不使用虚构的完成百分比。\n\n```css\n.prompt-card {\n  border-radius: 14px;\n  background: var(--card);\n}\n```" },
  ],
};

function Preview() {
  const [page, setPage] = useState<"sessions" | "conversation">(params.get("scene") === "list" ? "sessions" : "conversation");
  const [goal, setGoal] = useState(detail.goal);
  const showing = { ...detail, goal, ...(params.get("scene") === "approval" ? { questions: { requestId: "visual-question", questions: [
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
    mutate: async (_type, fields) => { if ("text" in fields && fields.text === "/goal clear") setGoal(null); return null; },
  };
  return <RemoteView app={app} previewOptions={options} />;
}
createRoot(document.getElementById("root")!).render(<Preview />);
