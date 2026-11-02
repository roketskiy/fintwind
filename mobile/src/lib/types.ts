export type SessionStatus =
  | "idle"
  | "connecting"
  | "working"
  | "waiting"
  | "failed";
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
  turnId?: string;
  turnRunning?: boolean;
  responseComplete?: boolean;
  streaming?: boolean;
  createdAt?: number;
  elapsedSeconds?: number;
  type?: "message";
  id: string;
  role: "user" | "assistant";
  text: string;
  attachments?: { name: string; isImage: boolean }[];
}
export interface RemoteActivity {
  turnId?: string;
  turnRunning?: boolean;
  type: "activity";
  id: string;
  kind: string;
  title: string;
  target: string;
  text: string;
  complete: boolean;
  failed: boolean;
  truncated: boolean;
}
export type TranscriptEntry = RemoteMessage | RemoteActivity;
export interface ComposerOptions {
  canConfigure: boolean;
  models: {
    id: string;
    name: string;
    provider: string;
    efforts: { id: string; label: string }[];
  }[];
  commands: { name: string; description: string }[];
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
export interface TodoSnapshot {
  items: { content: string; status: "pending" | "in_progress" | "completed" | "cancelled" }[];
  completed: number;
  total: number;
  truncated: boolean;
}
export interface SessionDetail {
  workspaceChanges?: { additions: number; deletions: number } | null;
  todos?: TodoSnapshot | null;
  goal?: { objective: string; status: "active" | "paused" | "blocked" | "done"; turns: number; maxTurns: number; reason: string; result: string; elapsedSeconds?: number; tools?: number } | null;
  id: string;
  projectId: string;
  title: string;
  status: SessionStatus;
  messages: RemoteMessage[];
  entries?: TranscriptEntry[];
  features: string[];
  model: string;
  modelName: string;
  interactionMode: string;
  reasoningEffort: string;
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
  watchSession: { fields: { sessionId: string; watchId: string; enabled: boolean }; result: { accepted: boolean } };
  getComposer: { fields: { sessionId: string }; result: ComposerOptions };
  configureSession: {
    fields: {
      sessionId: string;
      model?: string;
      interactionMode?: string;
      reasoningEffort?: string;
    };
    result: { accepted: boolean };
  };
  createSession: {
    fields: { projectId: string };
    result: { sessionId: string };
  };
  sendPrompt: {
    fields: { sessionId: string; text: string; attachmentIds?: string[] };
    result: { accepted: boolean };
  };
  uploadChunk: {
    fields: {
      sessionId: string;
      uploadId: string;
      name: string;
      offset: number;
      dataBase64: string;
      complete: boolean;
    };
    result: { accepted: boolean; offset: number };
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
