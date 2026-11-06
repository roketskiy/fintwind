CREATE TABLE `filesystem_operations` (
	`id` text PRIMARY KEY NOT NULL,
	`environment_id` text NOT NULL,
	`owner_id` text NOT NULL,
	`kind` text NOT NULL,
	`target_path` text NOT NULL,
	`state` text NOT NULL,
	`revision` integer NOT NULL,
	`created_at` integer NOT NULL,
	`updated_at` integer NOT NULL,
	`failure_kind` text,
	`exit_code` integer
);
--> statement-breakpoint
CREATE INDEX `filesystem_operations_by_environment` ON `filesystem_operations` (`environment_id`,`updated_at`);