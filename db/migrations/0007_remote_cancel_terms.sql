-- Durable terminal record for a remote prompt's cancellation.
--
-- The prompt row is not a place to keep an ending: it is deleted when a
-- session goes away, when a queue is swept, and once its outcome is
-- settled. A cancellation that has already been answered must stay
-- answerable — a device that asks again after a crash gets the recorded
-- ending, not a guess.
--
-- This table therefore holds only the ending, never any prompt text: the
-- cancel operation it answers, the prompt it was about, the submission the
-- daemon minted, the provenance the prompt was admitted under, and how the
-- wait ended. `state` is one of `cancelled`, `dispatched`, `expired`,
-- `notSent` or `unknown` — and `unknown` is a real, honest ending, not a
-- placeholder: it says the evidence ran out, and it is never upgraded to a
-- success by inference.
CREATE TABLE `remote_cancel_terms` (
	`cancel_operation_id` text PRIMARY KEY NOT NULL,
	`prompt_operation_id` text NOT NULL,
	`submission_id` text NOT NULL,
	`host_id` text NOT NULL,
	`device_id` text NOT NULL,
	`grant_version` integer NOT NULL,
	`state` text NOT NULL,
	`settled_at_ms` integer NOT NULL
);
--> statement-breakpoint
CREATE INDEX `remote_cancel_terms_by_prompt` ON `remote_cancel_terms` (`prompt_operation_id`);
