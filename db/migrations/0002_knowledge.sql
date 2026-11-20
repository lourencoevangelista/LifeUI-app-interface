CREATE TABLE "lifeui"."entities" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"iri" text NOT NULL,
	"class_iri" text NOT NULL,
	"name" text NOT NULL,
	"status" text,
	"sensitivity" text DEFAULT 'Personal' NOT NULL,
	"source_id" uuid,
	"confidence" numeric(3, 2),
	"props" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"origin" text DEFAULT 'import' NOT NULL,
	"recorded_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "entities_iri_unique" UNIQUE("iri"),
	CONSTRAINT "entities_status_check" CHECK ("lifeui"."entities"."status" IS NULL OR "lifeui"."entities"."status" IN ('Planned', 'InProgress', 'Completed', 'Cancelled', 'Blocked')),
	CONSTRAINT "entities_sensitivity_check" CHECK (sensitivity IN ('Public', 'Personal', 'Sensitive', 'Secret')),
	CONSTRAINT "entities_confidence_check" CHECK ("lifeui"."entities"."confidence" IS NULL OR ("lifeui"."entities"."confidence" >= 0 AND "lifeui"."entities"."confidence" <= 1)),
	CONSTRAINT "entities_origin_check" CHECK ("lifeui"."entities"."origin" IN ('import', 'app'))
);
--> statement-breakpoint
ALTER TABLE "lifeui"."entities" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE TABLE "lifeui"."entity_links" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"subject_iri" text NOT NULL,
	"predicate" text NOT NULL,
	"object_iri" text NOT NULL,
	"origin" text DEFAULT 'import' NOT NULL,
	CONSTRAINT "entity_links_triple_unique" UNIQUE("subject_iri","predicate","object_iri"),
	CONSTRAINT "entity_links_origin_check" CHECK ("lifeui"."entity_links"."origin" IN ('import', 'app'))
);
--> statement-breakpoint
ALTER TABLE "lifeui"."entity_links" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
ALTER TABLE "lifeui"."entities" ADD CONSTRAINT "entities_source_id_sources_id_fk" FOREIGN KEY ("source_id") REFERENCES "lifeui"."sources"("id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "entities_class_idx" ON "lifeui"."entities" USING btree ("class_iri");--> statement-breakpoint
CREATE INDEX "entity_links_object_idx" ON "lifeui"."entity_links" USING btree ("object_iri");--> statement-breakpoint
CREATE INDEX "entity_links_predicate_idx" ON "lifeui"."entity_links" USING btree ("predicate");