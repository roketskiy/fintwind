import type { ComposerOptions, SessionDetail } from "./types";

export type ComposerCommand = { name: string; description: string; value: string };

export function composerCommands(session: SessionDetail, options: ComposerOptions | null): ComposerCommand[] {
  const goals: ComposerCommand[] = session.features.includes("goals") ? [
    { name: "goal", description: "设置目标，或查看当前目标", value: "/goal " },
    ...(session.goal?.status === "active" ? [{ name: "goal pause", description: "暂停目标", value: "/goal pause" }] : []),
    ...(session.goal && ["paused", "blocked"].includes(session.goal.status) ? [{ name: "goal resume", description: "继续目标", value: "/goal resume" }] : []),
    ...(session.goal ? [{ name: "goal clear", description: "清除目标", value: "/goal clear" }] : []),
  ] : [];
  return [...goals, ...(options?.commands ?? [])
    .filter((command) => !session.features.includes("goals") || command.name.replace(/^\//, "").toLowerCase() !== "goal")
    .map((command) => ({ ...command, name: command.name.replace(/^\//, ""), value: `/${command.name.replace(/^\//, "")} ` }))];
}

// PromptBar's slash token, restricted to the leading command that the desktop executes.
export function slashQuery(draft: string): string | null {
  const match = /^\/([^\s]*|goal(?:\s+[^\s]*)?)$/i.exec(draft);
  return match ? match[1].toLowerCase() : null;
}
