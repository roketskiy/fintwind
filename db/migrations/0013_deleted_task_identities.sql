-- Keep deletion fences across daemon restarts. No transcript or credential.
CREATE TABLE `deleted_task_identities` (
  `id` text NOT NULL,
  `kind` text NOT NULL,
  `deleted_at` integer NOT NULL
);
--> statement-breakpoint
CREATE UNIQUE INDEX `deleted_task_identity` ON `deleted_task_identities` (`kind`,`id`);
