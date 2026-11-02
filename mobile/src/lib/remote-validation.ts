import type {
  CommandType,
  Permission,
  Questions,
  RemoteMessage,
  TranscriptEntry,
  SessionDetail,
  SessionStatus,
  SessionSummary,
  Workspace,
} from "./types";

export const REVISION_RE = /^[a-f0-9]{64}$/;
const SESSION_STATUSES: readonly SessionStatus[] = [
  "idle",
  "connecting",
  "working",
  "waiting",
  "failed",
];

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function reqStr(value: unknown, max: number, min = 1): string {
  if (typeof value !== "string" || value.length < min || value.length > max)
    throw new Error("响应格式无效");
  return value;
}

export function optStr(value: unknown, max: number): string {
  if (typeof value !== "string" || value.length > max)
    throw new Error("响应格式无效");
  return value;
}

export function reqNum(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value))
    throw new Error("响应格式无效");
  return value;
}

export function reqBool(value: unknown): boolean {
  if (typeof value !== "boolean") throw new Error("响应格式无效");
  return value;
}

export function reqArr<T>(
  value: unknown,
  max: number,
  parse: (entry: unknown) => T,
): T[] {
  if (!Array.isArray(value) || value.length > max)
    throw new Error("响应格式无效");
  return value.map(parse);
}

export function textBytes(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

export function mutationScope(
  type: CommandType,
  fields: { projectId?: string; sessionId?: string },
): string {
  if (
    type === "listWorkspaces" ||
    type === "listSessions" ||
    type === "getSession" ||
    type === "getComposer"
  )
    return "";
  if (fields.sessionId) return `session:${fields.sessionId}`;
  if (fields.projectId) return `project:${fields.projectId}`;
  return "";
}

export function checkFieldBounds(
  type: CommandType,
  fields: Record<string, unknown>,
): void {
  const bad = (): Error => new Error("请求参数无效");
  const id = (value: unknown): void => {
    if (typeof value !== "string" || !value || value.length > 256) throw bad();
  };
  switch (type) {
    case "listWorkspaces":
      return;
    case "listSessions":
    case "createSession":
      id(fields["projectId"]);
      return;
    case "getSession":
    case "getComposer":
    case "stop":
      id(fields["sessionId"]);
      return;
    case "configureSession":
      id(fields["sessionId"]);
      if (fields["model"] !== undefined) reqStr(fields["model"], 512);
      if (fields["reasoningEffort"] !== undefined)
        reqStr(fields["reasoningEffort"], 128);
      if (
        fields["interactionMode"] !== undefined &&
        !["build", "plan"].includes(String(fields["interactionMode"]))
      )
        throw bad();
      return;
    case "sendPrompt": {
      id(fields["sessionId"]);
      const attachments = fields["attachmentIds"] ?? [];
      if (!Array.isArray(attachments) || attachments.length > 8) throw bad();
      attachments.forEach(id);
      if (
        typeof fields["text"] !== "string" ||
        (!fields["text"].trim() && !attachments.length)
      )
        throw bad();
      if (textBytes(fields["text"]) > 32768)
        throw new Error("消息不能超过 32 KiB。");
      return;
    }
    case "uploadChunk":
      id(fields["sessionId"]);
      id(fields["uploadId"]);
      reqStr(fields["name"], 128);
      reqStr(fields["dataBase64"], 65536);
      reqBool(fields["complete"]);
      if (
        !Number.isSafeInteger(fields["offset"]) ||
        Number(fields["offset"]) < 0 ||
        Number(fields["offset"]) > 2796204
      )
        throw bad();
      return;
    case "respondPermission":
      id(fields["sessionId"]);
      reqStr(fields["requestId"], 256);
      reqStr(fields["optionId"], 256);
      return;
    case "answerQuestions": {
      id(fields["sessionId"]);
      reqStr(fields["requestId"], 256);
      const answers = fields["answers"];
      if (!Array.isArray(answers) || answers.length > 32) throw bad();
      let totalBytes = 0;
      for (const entry of answers) {
        if (!isRecord(entry)) throw bad();
        reqStr(entry["questionId"], 256);
        const list = entry["answers"];
        if (!Array.isArray(list) || list.length > 32) throw bad();
        for (const text of list) {
          if (
            typeof text !== "string" ||
            !text.trim() ||
            textBytes(text) > 4096
          )
            throw bad();
          totalBytes += textBytes(text);
          if (totalBytes > 65536) throw new Error("回答过长，请缩短后提交。");
        }
      }
    }
  }
}

export function parseWorkspace(value: unknown): Workspace {
  if (!isRecord(value)) throw new Error("响应格式无效");
  return { id: reqStr(value["id"], 512), name: reqStr(value["name"], 512) };
}

export function parseSessionSummary(value: unknown): SessionSummary {
  if (!isRecord(value)) throw new Error("响应格式无效");
  const status = reqStr(value["status"], 32);
  if (!(SESSION_STATUSES as readonly string[]).includes(status))
    throw new Error("响应格式无效");
  return {
    id: reqStr(value["id"], 256),
    projectId: reqStr(value["projectId"], 256),
    title: optStr(value["title"], 1024),
    status: status as SessionStatus,
    updatedAt: timestamp(value["updatedAt"]),
  };
}

export function parseRemoteMessage(value: unknown): RemoteMessage {
  if (!isRecord(value)) throw new Error("响应格式无效");
  const role = value["role"];
  if (role !== "user" && role !== "assistant") throw new Error("响应格式无效");
  return {
    id: reqStr(value["id"], 256),
    role,
    text: reqStr(value["text"], 65536, 0),
    attachments:
      value["attachments"] === undefined
        ? []
        : reqArr(value["attachments"], 8, (entry) => {
            if (!isRecord(entry)) throw new Error("响应格式无效");
            return {
              name: reqStr(entry["name"], 512),
              isImage: reqBool(entry["isImage"]),
            };
          }),
  };
}

export function parsePermission(value: unknown): Permission | null {
  if (value === null) return null;
  if (!isRecord(value)) throw new Error("响应格式无效");
  return {
    requestId: reqStr(value["requestId"], 256),
    title: optStr(value["title"], 4096),
    options: reqArr(value["options"], 32, (entry) => {
      if (!isRecord(entry)) throw new Error("响应格式无效");
      return {
        id: reqStr(entry["id"], 256),
        label: optStr(entry["label"], 1024),
        allow: reqBool(entry["allow"]),
      };
    }),
  };
}

function parseEntry(value: unknown): TranscriptEntry {
  if (!isRecord(value)) throw new Error("响应格式无效");
  if (value["type"] === "message")
    return { ...parseRemoteMessage(value), type: "message" };
  if (value["type"] !== "activity") throw new Error("响应格式无效");
  return {
    type: "activity",
    id: reqStr(value["id"], 256),
    kind: reqStr(value["kind"], 64),
    title: optStr(value["title"], 1024),
    target: value["target"] == null ? "" : optStr(value["target"], 2048),
    text: reqStr(value["text"], 65536, 0),
    complete: reqBool(value["complete"]),
    failed: reqBool(value["failed"]),
    truncated: reqBool(value["truncated"]),
  };
}

export function parseQuestions(value: unknown): Questions | null {
  if (value === null) return null;
  if (!isRecord(value)) throw new Error("响应格式无效");
  return {
    requestId: reqStr(value["requestId"], 256),
    questions: reqArr(value["questions"], 32, (entry) => {
      if (!isRecord(entry)) throw new Error("响应格式无效");
      return {
        id: reqStr(entry["id"], 256),
        header: optStr(entry["header"], 1024),
        question: optStr(entry["question"], 4096),
        multiSelect: reqBool(entry["multiSelect"]),
        options: reqArr(entry["options"], 64, (option) => {
          if (!isRecord(option)) throw new Error("响应格式无效");
          return {
            label: optStr(option["label"], 2048),
            description:
              option["description"] === null
                ? ""
                : optStr(option["description"], 4096),
          };
        }),
      };
    }),
  };
}

export function parseRevision(value: unknown): string | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || !REVISION_RE.test(value))
    throw new Error("响应格式无效");
  return value;
}

export function parseResult(type: CommandType, data: unknown): unknown {
  if (!isRecord(data)) throw new Error("响应格式无效");
  switch (type) {
    case "listWorkspaces": {
      const selected = data["selectedProjectId"];
      if (
        selected !== null &&
        !(typeof selected === "string" && selected.length <= 256)
      )
        throw new Error("响应格式无效");
      return {
        workspaces: reqArr(data["workspaces"], 4096, parseWorkspace),
        selectedProjectId: selected as string | null,
        revision: parseRevision(data["revision"]),
      };
    }
    case "listSessions":
      return {
        sessions: reqArr(data["sessions"], 256, parseSessionSummary),
        cutoff: reqNum(data["cutoff"]),
        revision: parseRevision(data["revision"]),
      };
    case "getSession": {
      const status = reqStr(data["status"], 32);
      if (!(SESSION_STATUSES as readonly string[]).includes(status))
        throw new Error("响应格式无效");
      const detail: SessionDetail = {
        id: reqStr(data["id"], 256),
        projectId: reqStr(data["projectId"], 256),
        title: optStr(data["title"], 1024),
        status: status as SessionStatus,
        messages: reqArr(data["messages"], 200, parseRemoteMessage),
        features:
          data["features"] === undefined
            ? []
            : reqArr(data["features"], 32, (entry) => reqStr(entry, 128)),
        model: data["model"] == null ? "" : optStr(data["model"], 512),
        modelName:
          data["modelName"] == null ? "" : optStr(data["modelName"], 1024),
        interactionMode:
          data["interactionMode"] == null
            ? ""
            : optStr(data["interactionMode"], 32),
        reasoningEffort:
          data["reasoningEffort"] == null
            ? ""
            : optStr(data["reasoningEffort"], 128),
        truncated: reqBool(data["truncated"]),
        permission: parsePermission(data["permission"]),
        questions: parseQuestions(data["questions"]),
        desktopRequired: reqBool(data["desktopRequired"]),
        canSend: reqBool(data["canSend"]),
      };
      const revision = parseRevision(data["revision"]);
      if (data["entries"] !== undefined)
        detail.entries = reqArr(data["entries"], 200, parseEntry);
      if (revision !== undefined) detail.revision = revision;
      return detail;
    }
    case "createSession":
      return { sessionId: reqStr(data["sessionId"], 256) };
    case "getComposer":
      return {
        canConfigure: reqBool(data["canConfigure"]),
        models: reqArr(data["models"], 1024, (entry) => {
          if (!isRecord(entry)) throw new Error("响应格式无效");
          return {
            id: reqStr(entry["id"], 512),
            name: optStr(entry["name"], 1024),
            provider:
              entry["provider"] == null ? "" : optStr(entry["provider"], 512),
            efforts: reqArr(entry["efforts"], 32, (effort) => {
              if (!isRecord(effort)) throw new Error("响应格式无效");
              return {
                id: reqStr(effort["id"], 128),
                label: optStr(effort["label"], 512),
              };
            }),
          };
        }),
        commands: reqArr(data["commands"], 64, (entry) => {
          if (!isRecord(entry)) throw new Error("响应格式无效");
          return {
            name: reqStr(entry["name"], 128),
            description: optStr(entry["description"], 1024),
          };
        }),
      };
    case "configureSession":
    case "sendPrompt":
    case "stop":
    case "respondPermission":
    case "answerQuestions":
      if (data["accepted"] !== true) throw new Error("响应格式无效");
      return { accepted: true };
    case "uploadChunk":
      if (data["accepted"] !== true) throw new Error("响应格式无效");
      return { accepted: true, offset: reqNum(data["offset"]) };
  }
}

function timestamp(value: unknown): number {
  const number = reqNum(value);
  if (!Number.isSafeInteger(number) || number < 0 || number > 8_640_000_000_000)
    throw new Error("响应格式无效");
  return number;
}
