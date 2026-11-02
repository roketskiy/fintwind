export type SessionStatus =
  "idle" | "connecting" | "working" | "waiting" | "failed";
export interface Workspace {
  id: string;
  name: string;
}
export interface SessionSummary {
  id: string;
  projectId: string;
  title: string;
  status: SessionStatus;
  updatedAt: number;
}
export interface RemoteMessage {
  id: string;
  role: "user" | "assistant";
  text: string;
}
export interface Permission {
  requestId: string;
  title: string;
  options: { id: string; label: string; allow: boolean }[];
}
export interface Question {
  id: string;
  header: string;
  question: string;
  multiSelect: boolean;
  options: { label: string; description: string }[];
}
export interface Questions {
  requestId: string;
  questions: Question[];
}
export interface SessionDetail {
  id: string;
  projectId: string;
  title: string;
  status: SessionStatus;
  messages: RemoteMessage[];
  truncated: boolean;
  permission: Permission | null;
  questions: Questions | null;
  desktopRequired: boolean;
  canSend: boolean;
  revision?: string;
}
export interface CommandMap {
  listWorkspaces: {
    fields: Record<string, never>;
    result: {
      workspaces: Workspace[];
      selectedProjectId: string | null;
      revision?: string;
    };
  };
  listSessions: {
    fields: { projectId: string };
    result: { sessions: SessionSummary[]; cutoff: number; revision?: string };
  };
  getSession: { fields: { sessionId: string }; result: SessionDetail };
  createSession: {
    fields: { projectId: string };
    result: { sessionId: string };
  };
  sendPrompt: {
    fields: { sessionId: string; text: string };
    result: { accepted: boolean };
  };
  stop: { fields: { sessionId: string }; result: { accepted: boolean } };
  respondPermission: {
    fields: { sessionId: string; requestId: string; optionId: string };
    result: { accepted: boolean };
  };
  answerQuestions: {
    fields: {
      sessionId: string;
      requestId: string;
      answers: { questionId: string; answers: string[] }[];
    };
    result: { accepted: boolean };
  };
}
export type CommandType = keyof CommandMap;
export type ConnectionPhase =
  | "idle"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "unpaired"
  | "error"
  | "offline";
export interface ClientSnapshot {
  connection: ConnectionPhase;
  error: string | null;
  pendingScopes: readonly string[];
}
