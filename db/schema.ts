/**
 * Fintwind local state schema.
 *
 * Drizzle is a build-time tool here: `bun run db:generate` diffs this file and
 * writes plain SQL into `db/migrations`, which the Rust app applies at startup
 * (see `apply_migrations` in `src/persistence.rs`). drizzle-orm never ships in
 * the binary — Rust owns every query.
 *
 * Session history is kept out of the `sessions` row: the row holds only what
 * the session list renders, so listing is a scan over narrow rows. The
 * transcript lives in `session_details` and messages in `messages`, both
 * fetched only when a session is opened.
 */

import { index, integer, sqliteTable, text } from "drizzle-orm/sqlite-core";

export const projects = sqliteTable("projects", {
  id: text("id").primaryKey(),
  name: text("name").notNull(),
  path: text("path").notNull(),
  /** Order shown in the sidebar. */
  position: integer("position").notNull(),
  /** When the project was added, unix seconds. */
  createdAt: integer("created_at").notNull(),
});

export const sessions = sqliteTable(
  "sessions",
  {
    id: text("id").primaryKey(),
    projectId: text("project_id").notNull(),
    /** Explicit user title; "New task" means the automatic fallback is active. */
    title: text("title").notNull(),
    /** Provider-generated title, with the first prompt as a local fallback. */
    autoTitle: text("auto_title"),
    provider: text("provider").notNull(),
    model: text("model"),
    status: text("status").notNull(),
    /** The session was discovered on the OpenCode server, not created here. */
    imported: integer("imported", { mode: "boolean" }).notNull().default(false),
    /** OpenCode's native session id — list-level, so reconciliation can match
     *  a skeleton to its server record without hydrating the transcript. */
    nativeSessionId: text("native_session_id"),
    /** Frozen Mermaid prompt version chosen when the session was created.
     *  NULL means the session predates the preference, so it stays off. */
    mermaidPromptVersion: text("mermaid_prompt_version"),
    /** Session creation time, unix seconds. */
    createdAt: integer("created_at").notNull(),
    /** Any mutation, unix seconds — including title edits and truncation. */
    updatedAt: integer("updated_at").notNull(),
    /** Completion of the most recent assistant turn, unix seconds. */
    lastReplyAt: integer("last_reply_at"),
    /** Tidied out of the active list; written by its own statement so an
     *  older projection can never wipe it (0005). */
    archived: integer("archived", { mode: "boolean" }).notNull().default(false),
  },
  (table) => [
    index("sessions_by_project").on(table.projectId, table.updatedAt),
    index("sessions_by_updated_at").on(table.updatedAt),
    index("sessions_by_last_reply_at").on(table.lastReplyAt),
  ],
);

/**
 * Conversation messages, one row each.
 *
 * Split out of `sessions.data` so appending to a long conversation writes one
 * small row instead of rewriting the whole history, and so a message can be
 * read or counted without deserializing a transcript.
 */
export const messages = sqliteTable(
  "messages",
  {
    id: text("id").primaryKey(),
    sessionId: text("session_id").notNull(),
    turnId: text("turn_id"),
    /** Ordinal within the session; conversation order, not wall-clock. */
    position: integer("position").notNull(),
    role: text("role").notNull(),
    content: text("content").notNull(),
    /** User-visible text before provider-facing attachment mentions. */
    displayContent: text("display_content"),
    /** JSON-serialized MessageAttachment array. */
    attachments: text("attachments").notNull().default("[]"),
    createdAt: integer("created_at").notNull(),
    streaming: integer("streaming", { mode: "boolean" }).notNull(),
  },
  (table) => [index("messages_by_session").on(table.sessionId, table.position)],
);

/**
 * The rest of `AgentSession` as JSON — transcript blocks, turns, provider
 * cursor.
 *
 * Split from `sessions` because it is large and rarely read: keeping it in the
 * row would mean listing sessions pages through every transcript, and every
 * title edit rewrites a transcript-sized row.
 */
export const sessionDetails = sqliteTable("session_details", {
  sessionId: text("session_id").primaryKey(),
  data: text("data").notNull(),
});

/** Immutable prompt identity plus independently durable admission receipt. */
export const submissions = sqliteTable(
  "submissions",
  {
    id: text("id").primaryKey(),
    sessionId: text("session_id").notNull(),
    state: text("state").notNull(),
    /** SubmissionRecord JSON; never included in diagnostic logs. */
    data: text("data").notNull(),
  },
  (table) => [index("submissions_by_session").on(table.sessionId)],
);

/**
 * Retired remote-control table (0005, 0006). No runtime code reads or
 * writes it. Retain this declaration and the applied migrations so schema
 * generation cannot silently delete existing user data.
 */
export const remotePrompts = sqliteTable(
  "remote_prompts",
  {
    operationId: text("operation_id").primaryKey(),
    submissionId: text("submission_id").notNull(),
    sessionId: text("session_id").notNull(),
    deviceId: text("device_id").notNull(),
    state: text("state").notNull(),
    summary: text("summary").notNull(),
    prompt: text("prompt").notNull(),
    enqueuedAtMs: integer("enqueued_at_ms").notNull(),
    expiresAtMs: integer("expires_at_ms").notNull(),
    hostId: text("host_id").notNull().default(""),
    grantVersion: integer("grant_version").notNull().default(0),
    cancelOperationId: text("cancel_operation_id"),
    cancelState: text("cancel_state"),
  },
  (table) => [index("remote_prompts_by_session").on(table.sessionId)],
);

/**
 * Retired remote-control table (0007); retained only for schema compatibility.
 */
export const remoteCancelTerms = sqliteTable(
  "remote_cancel_terms",
  {
    cancelOperationId: text("cancel_operation_id").primaryKey(),
    promptOperationId: text("prompt_operation_id").notNull(),
    submissionId: text("submission_id").notNull(),
    hostId: text("host_id").notNull(),
    deviceId: text("device_id").notNull(),
    grantVersion: integer("grant_version").notNull(),
    state: text("state").notNull(),
    settledAtMs: integer("settled_at_ms").notNull(),
  },
  (table) => [index("remote_cancel_terms_by_prompt").on(table.promptOperationId)],
);
