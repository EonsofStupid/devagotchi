CREATE TABLE `pet_actions` (
	`id` text PRIMARY KEY NOT NULL,
	`pet_id` text NOT NULL,
	`owner_id` text NOT NULL,
	`kind` text NOT NULL,
	`xp_delta` integer NOT NULL,
	`created_at` integer NOT NULL,
	FOREIGN KEY (`pet_id`) REFERENCES `pets`(`id`) ON UPDATE no action ON DELETE cascade,
	FOREIGN KEY (`owner_id`) REFERENCES `profiles`(`id`) ON UPDATE no action ON DELETE cascade
);
--> statement-breakpoint
CREATE INDEX `pet_actions_owner_created_idx` ON `pet_actions` (`owner_id`,`created_at`);--> statement-breakpoint
CREATE TABLE `pets` (
	`id` text PRIMARY KEY NOT NULL,
	`owner_id` text NOT NULL,
	`name` text NOT NULL,
	`species` text NOT NULL,
	`asset_key` text,
	`asset_type` text,
	`is_primary` integer DEFAULT false NOT NULL,
	`level` integer DEFAULT 1 NOT NULL,
	`xp` integer DEFAULT 0 NOT NULL,
	`bond` integer DEFAULT 20 NOT NULL,
	`energy` integer DEFAULT 80 NOT NULL,
	`focus` integer DEFAULT 50 NOT NULL,
	`streak` integer DEFAULT 1 NOT NULL,
	`last_care_at` integer,
	`created_at` integer NOT NULL,
	FOREIGN KEY (`owner_id`) REFERENCES `profiles`(`id`) ON UPDATE no action ON DELETE cascade
);
--> statement-breakpoint
CREATE INDEX `pets_owner_idx` ON `pets` (`owner_id`);--> statement-breakpoint
CREATE UNIQUE INDEX `pets_one_primary_idx` ON `pets` (`owner_id`) WHERE "pets"."is_primary" = 1;--> statement-breakpoint
CREATE TABLE `profiles` (
	`id` text PRIMARY KEY NOT NULL,
	`email` text NOT NULL,
	`display_name` text NOT NULL,
	`remember_until` integer,
	`notifications_enabled` integer DEFAULT false NOT NULL,
	`created_at` integer NOT NULL,
	`last_seen_at` integer NOT NULL
);
--> statement-breakpoint
CREATE UNIQUE INDEX `profiles_email_idx` ON `profiles` (`email`);--> statement-breakpoint
CREATE TABLE `push_subscriptions` (
	`id` text PRIMARY KEY NOT NULL,
	`owner_id` text NOT NULL,
	`endpoint` text NOT NULL,
	`subscription_json` text NOT NULL,
	`created_at` integer NOT NULL,
	FOREIGN KEY (`owner_id`) REFERENCES `profiles`(`id`) ON UPDATE no action ON DELETE cascade
);
--> statement-breakpoint
CREATE UNIQUE INDEX `push_endpoint_idx` ON `push_subscriptions` (`endpoint`);--> statement-breakpoint
CREATE INDEX `push_owner_idx` ON `push_subscriptions` (`owner_id`);