CREATE TABLE "lifeui"."audit_log" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"at" timestamp with time zone DEFAULT now() NOT NULL,
	"action" text NOT NULL,
	"target" text,
	"ip" text,
	"detail" jsonb DEFAULT '{}'::jsonb NOT NULL,
	CONSTRAINT "audit_log_action_check" CHECK (action IN ('setup_ok', 'setup_failed', 'login_ok', 'login_failed', 'login_locked', 'logout', 'event_create', 'critical_create', 'critical_read'))
);
--> statement-breakpoint
ALTER TABLE "lifeui"."audit_log" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE INDEX "audit_log_at_idx" ON "lifeui"."audit_log" USING btree ("at");--> statement-breakpoint
CREATE FUNCTION "lifeui"."audit_log_append_only"() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit_log is append-only (% refused)', TG_OP USING ERRCODE = 'restrict_violation'; END $$;--> statement-breakpoint
CREATE TRIGGER "audit_log_append_only" BEFORE UPDATE OR DELETE ON "lifeui"."audit_log" FOR EACH ROW EXECUTE FUNCTION "lifeui"."audit_log_append_only"();--> statement-breakpoint
CREATE TRIGGER "audit_log_no_truncate" BEFORE TRUNCATE ON "lifeui"."audit_log" FOR EACH STATEMENT EXECUTE FUNCTION "lifeui"."audit_log_append_only"();