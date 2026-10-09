-- Daemon-owned remote-control state: a durable prompt queue bound to the
-- submission store, and per-session archive state.
--
-- `remote_prompts` is the durable half of the remote queue. One row is one
-- admitted prompt: its operation id (what the phone knows), its submission
-- id (what the submission store knows), the session and device it belongs
-- to, and its prompt text. Writing a row and its submission share one
-- transaction, so a crash can never leave a prompt the host believes is
-- queued but the daemon has no record of.
CREATE TABLE `remote_prompts` (
	`operation_id` text PRIMARY KEY NOT NULL,
	`submission_id` text NOT NULL,
	`session_id` text NOT NULL,
	`device_id` text NOT NULL,
	`state` text NOT NULL,
	`summary` text NOT NULL,
	`prompt` text NOT NULL,
	`enqueued_at_ms` integer NOT NULL,
	`expires_at_ms` integer NOT NULL,
	`host_id` text NOT NULL DEFAULT '',
	`grant_version` integer NOT NULL DEFAULT 0
);
--> statement-breakpoint
CREATE INDEX `remote_prompts_by_session` ON `remote_prompts` (`session_id`);
--> statement-breakpoint
-- Archive is a list-level fact so the daemon can filter without hydrating a
-- transcript. The default keeps every existing row unarchived, and the
-- session JSON blob carries the same flag for detail reads.
ALTER TABLE `sessions` ADD COLUMN `archived` integer NOT NULL DEFAULT 0;
