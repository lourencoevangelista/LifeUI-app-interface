CREATE TABLE "lifeui"."agent_inbox" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"body" text NOT NULL,
	"status" text DEFAULT 'new' NOT NULL,
	"reply" text,
	"run_id" uuid,
	CONSTRAINT "agent_inbox_status_check" CHECK (status IN ('new', 'picked_up', 'done', 'needs_answer', 'cancelled')),
	CONSTRAINT "agent_inbox_body_check" CHECK (char_length("lifeui"."agent_inbox"."body") BETWEEN 1 AND 2000)
);
--> statement-breakpoint
ALTER TABLE "lifeui"."agent_inbox" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE TABLE "lifeui"."agent_messages" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"kind" text NOT NULL,
	"slot" text DEFAULT 'manual' NOT NULL,
	"title" text NOT NULL,
	"body" text NOT NULL,
	"origin" text DEFAULT 'manual' NOT NULL,
	"run_id" uuid,
	CONSTRAINT "agent_messages_kind_check" CHECK (kind IN ('briefing', 'overview', 'question', 'proposal', 'note')),
	CONSTRAINT "agent_messages_slot_check" CHECK (slot IN ('morning', 'midday', 'evening', 'manual')),
	CONSTRAINT "agent_messages_origin_check" CHECK (origin IN ('manual', 'agent')),
	CONSTRAINT "agent_messages_title_check" CHECK (char_length("lifeui"."agent_messages"."title") BETWEEN 1 AND 200),
	CONSTRAINT "agent_messages_body_check" CHECK (char_length("lifeui"."agent_messages"."body") BETWEEN 1 AND 8000)
);
--> statement-breakpoint
ALTER TABLE "lifeui"."agent_messages" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE TABLE "lifeui"."agent_runs" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"started_at" timestamp with time zone DEFAULT now() NOT NULL,
	"finished_at" timestamp with time zone,
	"slot" text NOT NULL,
	"status" text DEFAULT 'running' NOT NULL,
	"summary" text,
	"tool_calls" integer DEFAULT 0 NOT NULL,
	"tokens" integer DEFAULT 0 NOT NULL,
	CONSTRAINT "agent_runs_slot_check" CHECK (slot IN ('morning', 'midday', 'evening', 'manual')),
	CONSTRAINT "agent_runs_status_check" CHECK (status IN ('running', 'ok', 'failed', 'skipped'))
);
--> statement-breakpoint
ALTER TABLE "lifeui"."agent_runs" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
ALTER TABLE "lifeui"."audit_log" DROP CONSTRAINT "audit_log_action_check";--> statement-breakpoint
ALTER TABLE "lifeui"."agent_inbox" ADD CONSTRAINT "agent_inbox_run_id_agent_runs_id_fk" FOREIGN KEY ("run_id") REFERENCES "lifeui"."agent_runs"("id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "lifeui"."agent_messages" ADD CONSTRAINT "agent_messages_run_id_agent_runs_id_fk" FOREIGN KEY ("run_id") REFERENCES "lifeui"."agent_runs"("id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "agent_inbox_status_idx" ON "lifeui"."agent_inbox" USING btree ("status","created_at");--> statement-breakpoint
CREATE INDEX "agent_messages_created_idx" ON "lifeui"."agent_messages" USING btree ("created_at");--> statement-breakpoint
CREATE INDEX "agent_runs_started_idx" ON "lifeui"."agent_runs" USING btree ("started_at");--> statement-breakpoint
ALTER TABLE "lifeui"."audit_log" ADD CONSTRAINT "audit_log_action_check" CHECK (action IN ('setup_ok', 'setup_failed', 'login_ok', 'login_failed', 'login_locked', 'logout', 'event_create', 'critical_create', 'critical_read', 'agent_inbox_create', 'agent_inbox_update'));