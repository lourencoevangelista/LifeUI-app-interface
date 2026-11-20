CREATE SCHEMA "lifeui";
--> statement-breakpoint
CREATE TABLE "lifeui"."critical_information" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"iri" text NOT NULL,
	"name" text NOT NULL,
	"description" text,
	"category" text NOT NULL,
	"sensitivity" text DEFAULT 'Sensitive' NOT NULL,
	"storage_ref" text,
	"review_by" date,
	"source_id" uuid,
	"confidence" numeric(3, 2),
	"recorded_at" timestamp with time zone DEFAULT now() NOT NULL,
	"superseded_by" uuid,
	"payload" jsonb DEFAULT '{}'::jsonb NOT NULL,
	CONSTRAINT "critical_information_iri_unique" UNIQUE("iri"),
	CONSTRAINT "critical_category_check" CHECK (category IN ('Identity', 'Medical', 'Legal', 'Financial', 'Emergency', 'Access')),
	CONSTRAINT "critical_sensitivity_check" CHECK (sensitivity IN ('Public', 'Personal', 'Sensitive', 'Secret')),
	CONSTRAINT "critical_secret_has_ref_check" CHECK ("lifeui"."critical_information"."sensitivity" <> 'Secret' OR "lifeui"."critical_information"."storage_ref" IS NOT NULL),
	CONSTRAINT "critical_confidence_check" CHECK ("lifeui"."critical_information"."confidence" IS NULL OR ("lifeui"."critical_information"."confidence" >= 0 AND "lifeui"."critical_information"."confidence" <= 1))
);
--> statement-breakpoint
ALTER TABLE "lifeui"."critical_information" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE TABLE "lifeui"."events" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"iri" text NOT NULL,
	"class_iri" text NOT NULL,
	"name" text NOT NULL,
	"description" text,
	"occurred_at" timestamp with time zone,
	"occurred_on" date,
	"started_at" timestamp with time zone,
	"ended_at" timestamp with time zone,
	"status" text NOT NULL,
	"sensitivity" text DEFAULT 'Personal' NOT NULL,
	"source_id" uuid,
	"confidence" numeric(3, 2),
	"recorded_at" timestamp with time zone DEFAULT now() NOT NULL,
	"superseded_by" uuid,
	"payload" jsonb DEFAULT '{}'::jsonb NOT NULL,
	CONSTRAINT "events_iri_unique" UNIQUE("iri"),
	CONSTRAINT "events_has_time_check" CHECK ("lifeui"."events"."occurred_at" IS NOT NULL OR "lifeui"."events"."occurred_on" IS NOT NULL OR "lifeui"."events"."started_at" IS NOT NULL),
	CONSTRAINT "events_start_before_end_check" CHECK ("lifeui"."events"."started_at" IS NULL OR "lifeui"."events"."ended_at" IS NULL OR "lifeui"."events"."started_at" <= "lifeui"."events"."ended_at"),
	CONSTRAINT "events_status_check" CHECK (status IN ('Planned', 'InProgress', 'Completed', 'Cancelled', 'Blocked')),
	CONSTRAINT "events_sensitivity_check" CHECK (sensitivity IN ('Public', 'Personal', 'Sensitive', 'Secret')),
	CONSTRAINT "events_confidence_check" CHECK ("lifeui"."events"."confidence" IS NULL OR ("lifeui"."events"."confidence" >= 0 AND "lifeui"."events"."confidence" <= 1))
);
--> statement-breakpoint
ALTER TABLE "lifeui"."events" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE TABLE "lifeui"."sources" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"iri" text NOT NULL,
	"label" text NOT NULL,
	"kind" text NOT NULL,
	CONSTRAINT "sources_iri_unique" UNIQUE("iri"),
	CONSTRAINT "sources_kind_check" CHECK (kind IN ('UserStated', 'Observed', 'Imported', 'Inferred'))
);
--> statement-breakpoint
ALTER TABLE "lifeui"."sources" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
ALTER TABLE "lifeui"."critical_information" ADD CONSTRAINT "critical_information_source_id_sources_id_fk" FOREIGN KEY ("source_id") REFERENCES "lifeui"."sources"("id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "lifeui"."critical_information" ADD CONSTRAINT "critical_information_superseded_by_critical_information_id_fk" FOREIGN KEY ("superseded_by") REFERENCES "lifeui"."critical_information"("id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "lifeui"."events" ADD CONSTRAINT "events_source_id_sources_id_fk" FOREIGN KEY ("source_id") REFERENCES "lifeui"."sources"("id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "lifeui"."events" ADD CONSTRAINT "events_superseded_by_events_id_fk" FOREIGN KEY ("superseded_by") REFERENCES "lifeui"."events"("id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "critical_category_idx" ON "lifeui"."critical_information" USING btree ("category");--> statement-breakpoint
CREATE INDEX "events_class_idx" ON "lifeui"."events" USING btree ("class_iri");--> statement-breakpoint
CREATE INDEX "events_occurred_at_idx" ON "lifeui"."events" USING btree ("occurred_at");