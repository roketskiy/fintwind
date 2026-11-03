import type { ComposerOptions, Questions, SessionDetail, TaskActivity } from "../lib/types";

export const workflowEnabled = new URLSearchParams(location.search).has("workflow");
const params = new URLSearchParams(location.search);
const polish = params.has("polish");
let state: "idle" | "working" | "failed" | "permission" | "question" = "idle";
let event = 0;
let completed: TaskActivity["completed"] = { id: "old-completion", status: "completed", at: 1 };
let questionNumber = 0;
let model = polish ? "openai/sol" : "provider-a/alpha";
let brokenMarkdown = false;
let diagramVersion = 0;
export const workflowCounters = { activityReads: 0, answerSubmissions: 0 };
export const workflowCatalog: ComposerOptions = {
  canConfigure: true, favoriteModelIds: polish ? ["anthropic/opus", "openai/sol"] : ["provider-b/beta", "provider-a/alpha"], commands: [],
  models: polish ? [
    ...["GPT-6.1 Sol", "GPT-6.1 Terra", "GPT-6 Luna", "GPT-5.4", "GPT-5.4 mini", "GPT-5.4 nano", "GPT-5.3 Codex", "GPT-5.2", "OpenAI Compatible Very Long Model Name"].map((name, index) => ({ id: index === 0 ? "openai/sol" : `openai/model-${index}`, name, provider: "OpenAI", efforts: [] })),
    { id: "anthropic/opus", name: "Claude Opus", provider: "Anthropic", efforts: [] },
    { id: "google/gemini", name: "Gemini Pro", provider: "Google", efforts: [] },
    ...["DeepSeek", "Z.AI", "MiniMax", "Moonshot", "OpenRouter", "An Example Provider With A Long Name", "OpenCode"].map((provider, index) => ({ id: `other/${index}`, name: `Fixture Model ${index}`, provider, efforts: [] })),
  ] : [
    { id: "provider-a/alpha", name: "Alpha", provider: "Provider A", efforts: [] },
    { id: "provider-a/second", name: "Second", provider: "Provider A", efforts: [] },
    { id: "provider-b/beta", name: "Beta", provider: "Provider B", efforts: [] },
    { id: "opencode/auto", name: "Auto", provider: "OpenCode", efforts: [] },
  ],
};
const question = (): Questions => ({ requestId: `question-${questionNumber}`, questions: [{ id: "choice", header: "方向", question: "选择实现方式", multiSelect: false,
  options: [{ label: "方案 A", description: "保留现有结构" }, { label: "方案 B", description: "使用新的结构" }] }] });
const richMarkdown = [
  "行内公式 $E=mc^2$。",
  "$$\n\\int_0^1 x^2\\,dx=\\frac{1}{3}\n$$",
  "```mermaid\nflowchart LR\n  A[开始] --> B[完成]\n```",
  "`$not_math$` 保留代码。",
  "```text\n$not_math$\n```",
].join("\n\n");
const mathMarkdown = String.raw`### 行内公式
$E=mc^2$ 与 \(a^2+b^2=c^2\)。

### 块级公式
$$
\int_0^1 x^2\,dx=\frac{1}{3}
$$

\[\frac{1}{n}\sum_{i=1}^{n}x_i\]

$$\lim_{x\to0}\frac{\sin x}{x}=1$$

\[
\begin{bmatrix}1 & 2 \\ 3 & 4\end{bmatrix}
\]

\[
f(x)=\begin{cases}x^2 & x\ge0 \\ -x & x<0\end{cases}
\]

### 方程环境
\begin{align}
x+y &= 3 \\
x-y &= 1
\end{align}

\begin{equation}
e^{i\pi}+1=0
\end{equation}

### 宽公式
\[
\mathcal{F}(\omega)=\frac{1}{2\pi}\int_{-\infty}^{+\infty}\frac{\sum_{n=0}^{\infty}a_n x^n+\sqrt{1+x^2}}{\left(1+x^2\right)^{3/2}+\frac{\alpha}{1+\beta x^2}}\,e^{-i\omega x}\,dx
\]

> \[
> \sqrt{x^2+y^2}
> \]

### 代码保持原样
` + "`\\(not_math\\)`\n\n```math\n\\sum_{k=1}^{n}k=\\frac{n(n+1)}{2}\n```\n\n```js\nconst tex = '\\[not_math\\]';\n```\n\n    \\(indented_code\\)\n\n" + String.raw`双反斜杠 \\(escaped_text\\) 不应作为公式。

未闭合的 \[\frac{1}{2} 不应吞掉后面的文字。
`;
function diagramMarkdown() {
  if (params.get("diagram") === "sequence") return "```mermaid\nsequenceDiagram\n  participant U as 用户\n  participant A as 应用\n  participant S as 服务\n  U->>A: 发送请求\n  A->>S: 处理\n  S-->>A: 返回结果\n  A-->>U: 展示\n```";
  if (params.get("diagram") === "theme") return "```mermaid\nflowchart LR\n  A[开始] --> B[完成]\n  style B fill:var(--ondine-surface),stroke:var(--ondine-accent),color:var(--ondine-fg)\n```";
  return "```mermaid\nflowchart LR\n" + Array.from({ length: 24 }, (_, index) => `N${index}[步骤 ${index + 1}] --> N${index + 1}[步骤 ${index + 2}]`).join("\n") + `\nN24 --> Done[完成 ${diagramVersion}]\n` + "```";
}
export function workflowDetail(sessionId: string): SessionDetail {
  const other = sessionId === "other-session";
  return { id: sessionId, projectId: other ? "other-project" : "fixture-project", title: other ? "其他工作区任务" : "工作流检查", status: state === "permission" || state === "question" ? "waiting" : state,
    model, modelName: workflowCatalog.models.find(item => item.id === model)?.name ?? "Alpha", interactionMode: "build", reasoningEffort: "high", features: ["composerOptions", "sessionDeltas", "sessionPermissions"],
    truncated: false, desktopRequired: false, canSend: true, messages: [], runtimeMode: "ask",
    failureReason: state === "failed" ? "HTTP 429: request quota exceeded\nBearer fixture-sensitive-token https://private.invalid?api_key=fixture-secret" : null,
    permission: state === "permission" ? { requestId: `permission-${event}`, title: "允许读取文件？", options: [{ id: "allow", label: "允许", allow: true }] } : null,
    questions: state === "question" ? question() : null,
    entries: [ { id: "wf-prompt", role: "user", text: "隔离工作流检查" }, { id: "wf-reply", role: "assistant", streaming: false,
      text: brokenMarkdown ? "$$\\invalidcommand{x}$$\n\n$\\href{javascript:alert(1)}{unsafe}$\n\n$\\includegraphics{https://invalid.test/unsafe.svg}$\n\n```mermaid\nflowchart LR\nA -->\n```\n\n```mermaid\nflowchart LR\nA[Safe]\nclick A \"https://invalid.test\"\n```" : params.has("math") ? mathMarkdown : params.has("diagram") ? diagramMarkdown() : polish ? "模型按提供商分类，收藏保持桌面端顺序。" : richMarkdown } ],
  };
}
export function workflowTasks(): TaskActivity[] {
  workflowCounters.activityReads++;
  return [{ id: "other-session", projectId: "other-project", title: "其他工作区任务", completed,
    permissionId: state === "permission" ? `permission-${event}` : null,
    questionId: state === "question" ? `question-${questionNumber}` : null }];
}
export async function workflowAnswer() {
  workflowCounters.answerSubmissions++;
  await new Promise<void>(resolve => window.setTimeout(resolve, 200));
  state = "idle";
}
export const workflowActions = {
  workflowStart: () => { state = "working"; },
  workflowDone: () => { state = "idle"; completed = { id: `done-${++event}`, status: "completed", at: Math.floor(Date.now() / 1000) }; },
  workflowFailed: () => { state = "failed"; completed = { id: `failed-${++event}`, status: "failed", at: Math.floor(Date.now() / 1000) }; },
  workflowPermission: () => { event++; state = "permission"; },
  workflowQuestion: () => { questionNumber++; state = "question"; },
  workflowIdle: () => { state = "idle"; },
  workflowBadMarkdown: () => { brokenMarkdown = true; },
  workflowDiagramNext: () => { diagramVersion++; },
};
export function workflowSelectModel(value: string) { model = value; }
