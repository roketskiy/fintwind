CREATE TABLE `submissions` (
	`id` text PRIMARY KEY NOT NULL,
	`session_id` text NOT NULL,
	`state` text NOT NULL,
	`data` text NOT NULL
);
--> statement-breakpoint
CREATE INDEX `submissions_by_session` ON `submissions` (`session_id`);