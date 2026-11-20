CREATE TABLE "lifeui"."agent_settings" (
	"id" smallint PRIMARY KEY DEFAULT 1 NOT NULL,
	"enabled" boolean DEFAULT false NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "agent_settings_single_row_check" CHECK ("lifeui"."agent_settings"."id" = 1)
);
--> statement-breakpoint
ALTER TABLE "lifeui"."agent_settings" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE TABLE "lifeui"."agent_tokens" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"label" text NOT NULL,
	"token_hash" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"last_used_at" timestamp with time zone,
	"revoked_at" timestamp with time zone,
	CONSTRAINT "agent_tokens_token_hash_unique" UNIQUE("token_hash"),
	CONSTRAINT "agent_tokens_label_check" CHECK (char_length("lifeui"."agent_tokens"."label") BETWEEN 1 AND 60)
);
--> statement-breakpoint
ALTER TABLE "lifeui"."agent_tokens" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
ALTER TABLE "lifeui"."audit_log" DROP CONSTRAINT "audit_log_action_check";--> statement-breakpoint
ALTER TABLE "lifeui"."audit_log" ADD CONSTRAINT "audit_log_action_check" CHECK (action IN ('setup_ok', 'setup_failed', 'login_ok', 'login_failed', 'login_locked', 'logout', 'event_create', 'critical_create', 'critical_read', 'agent_inbox_create', 'agent_inbox_update', 'agent_switch', 'agent_token_create', 'agent_token_revoke', 'agent_run'));