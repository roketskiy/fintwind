CREATE TABLE `scheduled_occurrences` (
	`id` text PRIMARY KEY NOT NULL,
	`task_id` text NOT NULL,
	`due_at` integer NOT NULL,
	`state` text NOT NULL,
	`manual` integer NOT NULL,
	`data` text NOT NULL
);
--> statement-breakpoint
CREATE INDEX `scheduled_occurrences_by_task` ON `scheduled_occurrences` (`task_id`,`due_at`);--> statement-breakpoint
CREATE UNIQUE INDEX `scheduled_occurrences_one_open` ON `scheduled_occurrences` (`task_id`) WHERE "state" <> 'settled';--> statement-breakpoint
CREATE UNIQUE INDEX `scheduled_occurrences_one_automatic` ON `scheduled_occurrences` (`task_id`,`due_at`) WHERE "manual" = 0;--> statement-breakpoint
CREATE TABLE `scheduled_tasks` (
	`id` text PRIMARY KEY NOT NULL,
	`revision` integer NOT NULL,
	`enabled` integer NOT NULL,
	`next_due_at` integer,
	`data` text NOT NULL
);
--> statement-breakpoint
CREATE INDEX `scheduled_tasks_by_next_due` ON `scheduled_tasks` (`enabled`,`next_due_at`);