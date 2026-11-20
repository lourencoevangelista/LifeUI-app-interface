import { sql } from "drizzle-orm";
import {
  boolean,
  check,
  date,
  index,
  integer,
  jsonb,
  numeric,
  pgSchema,
  smallint,
  text,
  timestamp,
  uuid,
  type AnyPgColumn,
  unique,
} from "drizzle-orm/pg-core";

/**
 * Persistence for the ontology class marked with life:dbTable (ontology/lifeui.ttl): life:Event -> events.
 * (The Vault, life:CriticalInformation -> critical_information, was removed in migration 0006; such items now live
 * in the knowledge base as entities.)
 * Vocabulary values below mirror the controlled vocabularies in the ontology; the CHECK
 * constraints mirror the SHACL shapes that can be enforced per row. Everything else lives in
 * `payload` (JSONB) and is validated by SHACL before it is written.
 *
 * Supabase hardening (this holds personal data):
 *  - tables live in their own `lifeui` schema, NOT `public`, because Supabase exposes `public`
 *    through its web (Data) API by default and does not enable row-level security on tables
 *    created by migrations;
 *  - `.enableRLS()` is switched on for every table with NO policies, so even if the schema is
 *    ever exposed, the publishable (anon) key can read and write nothing;
 *  - the application connects with the `postgres` role (bypasses RLS), so it is unaffected.
 */
export const lifeui = pgSchema("lifeui");

export const SENSITIVITY = ["Public", "Personal", "Sensitive", "Secret"] as const;
export const STATUS = ["Planned", "InProgress", "Completed", "Cancelled", "Blocked"] as const;
export const SOURCE_KIND = ["UserStated", "Observed", "Imported", "Inferred"] as const;

const inList = (column: string, values: readonly string[]) =>
  sql.raw(`${column} IN (${values.map((v) => `'${v}'`).join(", ")})`);

// ---------------------------------------------------------------------------------------------
// Login (single owner). Used by the Rust functions in api/auth_*.rs. RLS on, no policies, schema
// outside `public`: the browser-facing Supabase API can never read these tables.
// ---------------------------------------------------------------------------------------------

/** The owner's password as an Argon2id PHC string (never the password itself). One row only. */
export const ownerCredentials = lifeui
  .table(
    "owner_credentials",
    {
      id: smallint("id").primaryKey().default(1),
      passwordHash: text("password_hash").notNull(),
      createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    },
    (t) => [check("owner_credentials_single_row_check", sql`${t.id} = 1`)],
  )
  .enableRLS();

/** Brute-force counter shared by login and first-time setup. One row only. */
export const authThrottle = lifeui
  .table(
    "auth_throttle",
    {
      id: smallint("id").primaryKey().default(1),
      failedCount: integer("failed_count").notNull().default(0),
      lockedUntil: timestamp("locked_until", { withTimezone: true }),
      lastFailedAt: timestamp("last_failed_at", { withTimezone: true }),
    },
    (t) => [check("auth_throttle_single_row_check", sql`${t.id} = 1`)],
  )
  .enableRLS();

/** Live sessions. Only the SHA-256 of the cookie token is stored, so a database leak reveals no session. */
export const sessions = lifeui
  .table(
    "sessions",
    {
      tokenHash: text("token_hash").primaryKey(),
      createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
      expiresAt: timestamp("expires_at", { withTimezone: true }).notNull(),
      lastUsedAt: timestamp("last_used_at", { withTimezone: true }),
    },
    (t) => [index("sessions_expires_at_idx").on(t.expiresAt)],
  )
  .enableRLS();

/**
 * What the audit log records. Mirrors AUDIT_ACTIONS in src/store.rs (`cargo test` fails if they drift).
 * critical_create / critical_read belong to the removed Vault; they stay so older audit rows remain valid.
 */
export const AUDIT_ACTIONS = [
  "setup_ok",
  "setup_failed",
  "login_ok",
  "login_failed",
  "login_locked",
  "logout",
  "event_create",
  "critical_create",
  "critical_read",
  "agent_inbox_create",
  "agent_inbox_update",
  "agent_switch",
  "agent_token_create",
  "agent_token_revoke",
  "agent_run",
  "agent_stay_awake",
  "news_publish",
  "news_item_update",
  "news_missing",
] as const;

/**
 * Append-only audit trail: who did what and when. Never holds secrets or content, only the action, the
 * target identifier, the caller's IP address and a small detail object (counts, filters). A trigger refuses
 * UPDATE and DELETE (see the migration), so history cannot be rewritten through the app.
 */
export const auditLog = lifeui
  .table(
    "audit_log",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      at: timestamp("at", { withTimezone: true }).notNull().defaultNow(),
      action: text("action").notNull(),
      /** identifier of the thing acted on (event id, news item id...), or null */
      target: text("target"),
      ip: text("ip"),
      detail: jsonb("detail").notNull().default({}),
    },
    (t) => [
      check("audit_log_action_check", inList("action", AUDIT_ACTIONS)),
      index("audit_log_at_idx").on(t.at),
    ],
  )
  .enableRLS();

/** life:Source - where a fact came from. */
export const sources = lifeui
  .table(
    "sources",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      iri: text("iri").notNull().unique(),
      label: text("label").notNull(),
      kind: text("kind").notNull(),
    },
    () => [check("sources_kind_check", inList("kind", SOURCE_KIND))],
  )
  .enableRLS();

/** life:Event and subclasses. Class is kept in class_iri (e.g. .../ontology#Milestone). */
export const events = lifeui
  .table(
    "events",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      iri: text("iri").notNull().unique(),
      classIri: text("class_iri").notNull(),
      name: text("name").notNull(),
      description: text("description"),
      // SHACL EventShape: at least one of occurred_at / occurred_on / started_at.
      occurredAt: timestamp("occurred_at", { withTimezone: true }),
      occurredOn: date("occurred_on"),
      startedAt: timestamp("started_at", { withTimezone: true }),
      endedAt: timestamp("ended_at", { withTimezone: true }),
      status: text("status").notNull(),
      sensitivity: text("sensitivity").notNull().default("Personal"),
      sourceId: uuid("source_id").references(() => sources.id),
      confidence: numeric("confidence", { precision: 3, scale: 2 }),
      recordedAt: timestamp("recorded_at", { withTimezone: true }).notNull().defaultNow(),
      supersededBy: uuid("superseded_by").references((): AnyPgColumn => events.id),
      payload: jsonb("payload").notNull().default({}),
    },
    (t) => [
      check("events_has_time_check", sql`${t.occurredAt} IS NOT NULL OR ${t.occurredOn} IS NOT NULL OR ${t.startedAt} IS NOT NULL`),
      check("events_start_before_end_check", sql`${t.startedAt} IS NULL OR ${t.endedAt} IS NULL OR ${t.startedAt} <= ${t.endedAt}`),
      check("events_status_check", inList("status", STATUS)),
      check("events_sensitivity_check", inList("sensitivity", SENSITIVITY)),
      check("events_confidence_check", sql`${t.confidence} IS NULL OR (${t.confidence} >= 0 AND ${t.confidence} <= 1)`),
      index("events_class_idx").on(t.classIri),
      index("events_occurred_at_idx").on(t.occurredAt),
    ],
  )
  .enableRLS();

// ---------------------------------------------------------------------------------------------
// Everything else in the knowledge base (documents, projects, tasks, courses, people, tools...).
// A PROJECTION of the Turtle files (data-private/*.ttl are the master): `tools/import_knowledge.py`
// refreshes rows whose origin is 'import' inside one transaction. Events are NOT here: they live in
// their own typed table above and are never overwritten by an import.
// ---------------------------------------------------------------------------------------------

/** One row per ontology entity that is not an Event or Source (CriticalInformation pointers included). */
export const entities = lifeui
  .table(
    "entities",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      iri: text("iri").notNull().unique(),
      classIri: text("class_iri").notNull(),
      name: text("name").notNull(),
      status: text("status"),
      sensitivity: text("sensitivity").notNull().default("Personal"),
      sourceId: uuid("source_id").references(() => sources.id),
      confidence: numeric("confidence", { precision: 3, scale: 2 }),
      /** every other literal property (description, dates, role, value, unit, storageRef...), keyed by property name */
      props: jsonb("props").notNull().default({}),
      origin: text("origin").notNull().default("import"),
      recordedAt: timestamp("recorded_at", { withTimezone: true }).notNull().defaultNow(),
    },
    (t) => [
      check("entities_status_check", sql`${t.status} IS NULL OR ${t.status} IN ('Planned', 'InProgress', 'Completed', 'Cancelled', 'Blocked')`),
      check("entities_sensitivity_check", inList("sensitivity", SENSITIVITY)),
      check("entities_confidence_check", sql`${t.confidence} IS NULL OR (${t.confidence} >= 0 AND ${t.confidence} <= 1)`),
      check("entities_origin_check", sql`${t.origin} IN ('import', 'app')`),
      index("entities_class_idx").on(t.classIri),
    ],
  )
  .enableRLS();

/** Relations between entities, events, critical items and sources: (subject, predicate, object). */
export const entityLinks = lifeui
  .table(
    "entity_links",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      subjectIri: text("subject_iri").notNull(),
      /** ontology property name without namespace, e.g. relatesTo, partOf, usesTool */
      predicate: text("predicate").notNull(),
      objectIri: text("object_iri").notNull(),
      origin: text("origin").notNull().default("import"),
    },
    (t) => [
      unique("entity_links_triple_unique").on(t.subjectIri, t.predicate, t.objectIri),
      check("entity_links_origin_check", sql`${t.origin} IN ('import', 'app')`),
      index("entity_links_object_idx").on(t.objectIri),
      index("entity_links_predicate_idx").on(t.predicate),
    ],
  )
  .enableRLS();

// ---------------------------------------------------------------------------------------------
// The scheduled agent (Agent tab). A work queue and a message feed, NOT knowledge: unlike events and
// critical information these rows may change status in place. The values mirror AGENT_* in src/agent.rs
// (`cargo test` fails on drift). Content is never copied into the audit log.
// ---------------------------------------------------------------------------------------------

export const AGENT_SLOTS = ["morning", "midday", "evening", "manual"] as const;
export const AGENT_INBOX_STATUS = ["new", "picked_up", "done", "needs_answer", "cancelled"] as const;
export const AGENT_MESSAGE_KINDS = ["briefing", "overview", "question", "proposal", "note"] as const;
export const AGENT_RUN_STATUS = ["running", "ok", "failed", "skipped"] as const;
/** An event the agent proposes from an inbox item: waits for the owner, who adds it (as an event) or dismisses it. */
export const AGENT_PROPOSAL_STATUS = ["pending", "added", "dismissed"] as const;

/** One scheduled run of the agent (read-only in the app; written by the runner). */
export const agentRuns = lifeui
  .table(
    "agent_runs",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      startedAt: timestamp("started_at", { withTimezone: true }).notNull().defaultNow(),
      finishedAt: timestamp("finished_at", { withTimezone: true }),
      slot: text("slot").notNull(),
      status: text("status").notNull().default("running"),
      summary: text("summary"),
      toolCalls: integer("tool_calls").notNull().default(0),
      tokens: integer("tokens").notNull().default(0),
    },
    (t) => [
      check("agent_runs_slot_check", inList("slot", AGENT_SLOTS)),
      check("agent_runs_status_check", inList("status", AGENT_RUN_STATUS)),
      index("agent_runs_started_idx").on(t.startedAt),
    ],
  )
  .enableRLS();

/** Instructions from the owner to the agent ("add dentist Friday 15:00"). */
export const agentInbox = lifeui
  .table(
    "agent_inbox",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
      updatedAt: timestamp("updated_at", { withTimezone: true }).notNull().defaultNow(),
      body: text("body").notNull(),
      status: text("status").notNull().default("new"),
      /** the agent's answer or question back, once it has handled the item */
      reply: text("reply"),
      runId: uuid("run_id").references((): AnyPgColumn => agentRuns.id),
      /** an event the agent prepared from this item ({class, name, date, time?, description?}); only TEXT until the
       * owner presses Add, which creates the event with the owner's login (the runner can never write events) */
      proposal: jsonb("proposal"),
      proposalStatus: text("proposal_status"),
      /** the event created from the proposal */
      proposalEventId: uuid("proposal_event_id").references((): AnyPgColumn => events.id),
    },
    (t) => [
      check("agent_inbox_status_check", inList("status", AGENT_INBOX_STATUS)),
      check("agent_inbox_proposal_status_check", inList("proposal_status", AGENT_PROPOSAL_STATUS)),
      check("agent_inbox_proposal_pair_check", sql`(${t.proposal} IS NULL) = (${t.proposalStatus} IS NULL)`),
      check("agent_inbox_proposal_event_check", sql`${t.proposalEventId} IS NULL OR ${t.proposalStatus} = 'added'`),
      check("agent_inbox_body_check", sql`char_length(${t.body}) BETWEEN 1 AND 2000`),
      index("agent_inbox_status_idx").on(t.status, t.createdAt),
    ],
  )
  .enableRLS();

/** What the agent says to the owner: briefings, evening overviews, questions, proposals. */
export const agentMessages = lifeui
  .table(
    "agent_messages",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
      kind: text("kind").notNull(),
      slot: text("slot").notNull().default("manual"),
      title: text("title").notNull(),
      body: text("body").notNull(),
      /** 'manual' = typed in through the app for testing; later 'agent' */
      origin: text("origin").notNull().default("manual"),
      runId: uuid("run_id").references((): AnyPgColumn => agentRuns.id),
    },
    (t) => [
      check("agent_messages_kind_check", inList("kind", AGENT_MESSAGE_KINDS)),
      check("agent_messages_slot_check", inList("slot", AGENT_SLOTS)),
      check("agent_messages_origin_check", inList("origin", ["manual", "agent"])),
      check("agent_messages_title_check", sql`char_length(${t.title}) BETWEEN 1 AND 200`),
      check("agent_messages_body_check", sql`char_length(${t.body}) BETWEEN 1 AND 8000`),
      index("agent_messages_created_idx").on(t.createdAt),
    ],
  )
  .enableRLS();

/**
 * The agent's kill switch. One row (created on first use); the agent starts OFF. While it is off the runner API
 * refuses every call, so the owner can stop the agent instantly from the Agent tab.
 */
export const agentSettings = lifeui
  .table(
    "agent_settings",
    {
      id: smallint("id").primaryKey().default(1),
      enabled: boolean("enabled").notNull().default(false),
      /** "Stay awake tonight" (News tab): the home server skips its evening shutdown on this date (local time). */
      stayAwakeOn: date("stay_awake_on"),
      updatedAt: timestamp("updated_at", { withTimezone: true }).notNull().defaultNow(),
    },
    (t) => [check("agent_settings_single_row_check", sql`${t.id} = 1`)],
  )
  .enableRLS();

/**
 * Credentials for the scheduled runner (cloud job or PC task). Only the SHA-256 of the token is stored, so a
 * database leak reveals no token. A token works on /api/runner only (never on the owner's endpoints) and can
 * be revoked at any time; revoked rows stay for the record.
 */
export const agentTokens = lifeui
  .table(
    "agent_tokens",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      label: text("label").notNull(),
      tokenHash: text("token_hash").notNull().unique(),
      createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
      lastUsedAt: timestamp("last_used_at", { withTimezone: true }),
      revokedAt: timestamp("revoked_at", { withTimezone: true }),
    },
    (t) => [check("agent_tokens_label_check", sql`char_length(${t.label}) BETWEEN 1 AND 60`)],
  )
  .enableRLS();

// ---------------------------------------------------------------------------------------------
// News tab: the daily news briefing about the owner's interests, built by the home server (tools/news/) and published through
// POST /api/runner/news with a runner token. Operational data like the agent tables: a briefing for a date is
// replaced when it is published again, and the owner's read/saved/dismissed marks survive the replacement.
// ---------------------------------------------------------------------------------------------

export const NEWS_BRIEFING_TYPES = ["daily", "weekly"] as const;
export const NEWS_TAGS = ["ai_breakthrough", "release", "research", "focus_relevant", "industry"] as const;

/** One briefing per date and type: intro, weather, the server status block. */
export const newsBriefings = lifeui
  .table(
    "news_briefings",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      date: date("date").notNull(),
      type: text("type").notNull().default("daily"),
      intro: text("intro").notNull().default(""),
      /** {date, tmin, tmax, precip_prob, wind, weather_type_pt, hourly[], warnings[], summary, stale...} from the weather fetcher (tools/news/weather.py) */
      weather: jsonb("weather"),
      /** updates, reboot, disks, SMART, last backup, Syncthing, GPU, failed steps, alerts */
      serverStatus: jsonb("server_status"),
      generatedByModel: text("generated_by_model").notNull().default(""),
      generatedAt: timestamp("generated_at", { withTimezone: true }).notNull(),
      publishedAt: timestamp("published_at", { withTimezone: true }).notNull().defaultNow(),
      /** fetched/new/candidate counts and per-source status, for the "Server status" section */
      stats: jsonb("stats").notNull().default({}),
      /**
       * Optional blocks from the morning task: calendar (today's Google Calendar events), digest (weekly briefing:
       * item URLs in order, Focus highlights, trends) and feedback (the monthly source/topic reweighting report).
       */
      extras: jsonb("extras").notNull().default({}),
    },
    (t) => [
      unique("news_briefings_date_type_key").on(t.date, t.type),
      check("news_briefings_type_check", inList("type", NEWS_BRIEFING_TYPES)),
      check("news_briefings_intro_check", sql`char_length(${t.intro}) <= 2000`),
    ],
  )
  .enableRLS();

/** The items of a briefing. A URL appears once ever (the server's 30-day memory keeps repeats out). */
export const newsItems = lifeui
  .table(
    "news_items",
    {
      id: uuid("id").primaryKey().defaultRandom(),
      briefingId: uuid("briefing_id")
        .notNull()
        .references(() => newsBriefings.id, { onDelete: "cascade" }),
      position: smallint("position").notNull(),
      url: text("url").notNull().unique(),
      title: text("title").notNull(),
      source: text("source").notNull(),
      publishedAt: timestamp("published_at", { withTimezone: true }),
      tags: jsonb("tags").notNull().default([]),
      topics: jsonb("topics").notNull().default([]),
      importance: smallint("importance").notNull(),
      focusRelevance: smallint("focus_relevance").notNull(),
      summary: text("summary").notNull(),
      whyItMatters: text("why_it_matters").notNull().default(""),
      points: integer("points"),
      readAt: timestamp("read_at", { withTimezone: true }),
      savedAt: timestamp("saved_at", { withTimezone: true }),
      dismissedAt: timestamp("dismissed_at", { withTimezone: true }),
    },
    (t) => [
      check("news_items_importance_check", sql`${t.importance} BETWEEN 0 AND 10`),
      check("news_items_focus_check", sql`${t.focusRelevance} BETWEEN 0 AND 10`),
      check("news_items_url_check", sql`${t.url} ~ '^https?://' AND char_length(${t.url}) <= 2000`),
      check("news_items_title_check", sql`char_length(${t.title}) BETWEEN 1 AND 300`),
      check("news_items_summary_check", sql`char_length(${t.summary}) <= 1000`),
      index("news_items_briefing_idx").on(t.briefingId, t.position),
    ],
  )
  .enableRLS();
