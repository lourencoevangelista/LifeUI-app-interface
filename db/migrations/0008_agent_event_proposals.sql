ALTER TABLE "lifeui"."agent_inbox" ADD COLUMN "proposal" jsonb;--> statement-breakpoint
ALTER TABLE "lifeui"."agent_inbox" ADD COLUMN "proposal_status" text;--> statement-breakpoint
ALTER TABLE "lifeui"."agent_inbox" ADD COLUMN "proposal_event_id" uuid;--> statement-breakpoint
ALTER TABLE "lifeui"."agent_inbox" ADD CONSTRAINT "agent_inbox_proposal_event_id_events_id_fk" FOREIGN KEY ("proposal_event_id") REFERENCES "lifeui"."events"("id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "lifeui"."agent_inbox" ADD CONSTRAINT "agent_inbox_proposal_status_check" CHECK (proposal_status IN ('pending', 'added', 'dismissed'));--> statement-breakpoint
ALTER TABLE "lifeui"."agent_inbox" ADD CONSTRAINT "agent_inbox_proposal_pair_check" CHECK (("lifeui"."agent_inbox"."proposal" IS NULL) = ("lifeui"."agent_inbox"."proposal_status" IS NULL));--> statement-breakpoint
ALTER TABLE "lifeui"."agent_inbox" ADD CONSTRAINT "agent_inbox_proposal_event_check" CHECK ("lifeui"."agent_inbox"."proposal_event_id" IS NULL OR "lifeui"."agent_inbox"."proposal_status" = 'added');