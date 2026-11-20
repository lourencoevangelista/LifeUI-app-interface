CREATE TABLE "lifeui"."news_briefings" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"date" date NOT NULL,
	"type" text DEFAULT 'daily' NOT NULL,
	"intro" text DEFAULT '' NOT NULL,
	"weather" jsonb,
	"server_status" jsonb,
	"generated_by_model" text DEFAULT '' NOT NULL,
	"generated_at" timestamp with time zone NOT NULL,
	"published_at" timestamp with time zone DEFAULT now() NOT NULL,
	"stats" jsonb DEFAULT '{}'::jsonb NOT NULL,
	CONSTRAINT "news_briefings_date_type_key" UNIQUE("date","type"),
	CONSTRAINT "news_briefings_type_check" CHECK (type IN ('daily', 'weekly')),
	CONSTRAINT "news_briefings_intro_check" CHECK (char_length("lifeui"."news_briefings"."intro") <= 2000)
);
--> statement-breakpoint
ALTER TABLE "lifeui"."news_briefings" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
CREATE TABLE "lifeui"."news_items" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"briefing_id" uuid NOT NULL,
	"position" smallint NOT NULL,
	"url" text NOT NULL,
	"title" text NOT NULL,
	"source" text NOT NULL,
	"published_at" timestamp with time zone,
	"tags" jsonb DEFAULT '[]'::jsonb NOT NULL,
	"topics" jsonb DEFAULT '[]'::jsonb NOT NULL,
	"importance" smallint NOT NULL,
	"focus_relevance" smallint NOT NULL,
	"summary" text NOT NULL,
	"why_it_matters" text DEFAULT '' NOT NULL,
	"points" integer,
	"read_at" timestamp with time zone,
	"saved_at" timestamp with time zone,
	"dismissed_at" timestamp with time zone,
	CONSTRAINT "news_items_url_unique" UNIQUE("url"),
	CONSTRAINT "news_items_importance_check" CHECK ("lifeui"."news_items"."importance" BETWEEN 0 AND 10),
	CONSTRAINT "news_items_focus_check" CHECK ("lifeui"."news_items"."focus_relevance" BETWEEN 0 AND 10),
	CONSTRAINT "news_items_url_check" CHECK ("lifeui"."news_items"."url" ~ '^https?://' AND char_length("lifeui"."news_items"."url") <= 2000),
	CONSTRAINT "news_items_title_check" CHECK (char_length("lifeui"."news_items"."title") BETWEEN 1 AND 300),
	CONSTRAINT "news_items_summary_check" CHECK (char_length("lifeui"."news_items"."summary") <= 1000)
);
--> statement-breakpoint
ALTER TABLE "lifeui"."news_items" ENABLE ROW LEVEL SECURITY;--> statement-breakpoint
ALTER TABLE "lifeui"."news_items" ADD CONSTRAINT "news_items_briefing_id_news_briefings_id_fk" FOREIGN KEY ("briefing_id") REFERENCES "lifeui"."news_briefings"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "news_items_briefing_idx" ON "lifeui"."news_items" USING btree ("briefing_id","position");