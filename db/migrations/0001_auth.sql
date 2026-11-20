CREATE TABLE "lifeui"."auth_throttle" (
	"id" smallint PRIMARY KEY DEFAULT 1 NOT NULL,
	"failed_count" integer DEFAULT 0 NOT NULL,
	"locked_until" timestamp with time zone,
	"last_failed_at" timestamp with time zone,
	CONSTRAINT "auth_throttle_single_row_check" CHECK ("lifeui"."auth_throttle"."id" = 1)
);
--> statement-breakpoint
ALTER TABLE "lifeui"."auth_throttle" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE TABLE "lifeui"."owner_credentials" (
	"id" smallint PRIMARY KEY DEFAULT 1 NOT NULL,
	"password_hash" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "owner_credentials_single_row_check" CHECK ("lifeui"."owner_credentials"."id" = 1)
);
--> statement-breakpoint
ALTER TABLE "lifeui"."owner_credentials" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE TABLE "lifeui"."sessions" (
	"token_hash" text PRIMARY KEY NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"last_used_at" timestamp with time zone
);
--> statement-breakpoint
ALTER TABLE "lifeui"."sessions" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE INDEX "sessions_expires_at_idx" ON "lifeui"."sessions" USING btree ("expires_at");