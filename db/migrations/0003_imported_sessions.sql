ALTER TABLE `sessions` ADD `imported` integer DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE `sessions` ADD `native_session_id` text;