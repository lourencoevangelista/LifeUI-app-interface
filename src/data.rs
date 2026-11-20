//! PostgreSQL queries for events (the persisted, versioned class) and the read-only knowledge base (entities).
//!
//! - Inputs arrive already validated by `domain`; everything is still a bound parameter (never string-built SQL).
//! - Nothing is ever updated in place or deleted: a change is a NEW row that `supersedes` the old one, which
//!   keeps its row and gets `superseded_by` set (facts are versioned, not overwritten).
//! - Reads return JSON built by Postgres (`jsonb_build_object`), so no date/decimal/uuid types are needed here.
//! - Every query uses `.persistent(false)` (Supabase's transaction pooler forbids prepared statements).

use serde_json::Value;
use sqlx::PgPool;

use crate::{
    domain::{ID_BASE, ONTOLOGY_BASE, ValidEvent},
    store,
};

/// Source recorded for facts entered through the app (see life:Source in the ontology).
pub const APP_SOURCE_IRI: &str = "https://lifeui.example/id/src-app";

#[derive(Debug)]
pub enum WriteError {
    /// `supersedes` names nothing, or an item that has already been replaced.
    SupersedeConflict,
    /// `from_proposal` names no Agent inbox item with a PENDING proposal (already added or dismissed, or none).
    ProposalConflict,
    /// A database CHECK rule refused the row (the last line of defence behind `domain`).
    Rejected(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for WriteError {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &error {
            if db.code().as_deref() == Some("23514") {
                return WriteError::Rejected(db.constraint().unwrap_or("check constraint").to_string());
            }
        }
        WriteError::Db(error)
    }
}

fn json(text: String) -> Value {
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

const EVENT_JSON: &str = "jsonb_build_object('id', e.id::text, 'iri', e.iri, 'class', regexp_replace(e.class_iri, '^.*#', ''), \
    'name', e.name, 'description', e.description, 'status', e.status, 'occurred_at', e.occurred_at, 'occurred_on', e.occurred_on, \
    'started_at', e.started_at, 'ended_at', e.ended_at, 'sensitivity', e.sensitivity, 'confidence', e.confidence::float8, \
    'recorded_at', e.recorded_at, 'superseded_by', e.superseded_by::text, 'payload', e.payload)";

const EVENT_SORT_KEY: &str = "COALESCE(e.occurred_at, e.started_at, e.occurred_on::timestamptz)";

async fn ensure_app_source(tx: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO lifeui.sources (iri, label, kind) VALUES ($1, 'Entered in the LifeUI app', 'UserStated') ON CONFLICT (iri) DO NOTHING")
        .bind(APP_SOURCE_IRI)
        .persistent(false)
        .execute(tx)
        .await?;
    Ok(())
}

// ------------------------------------------------------------------------------------ events

pub struct EventFilter {
    pub class: Option<String>, // short class name, already checked against EVENT_CLASSES
    pub status: Option<String>,
    pub from: Option<String>, // date or RFC 3339 instant, already validated
    pub to: Option<String>,   // exclusive upper bound
    pub limit: i64,
    pub include_superseded: bool,
}

pub async fn create_event(pool: &PgPool, event: &ValidEvent, ip: Option<&str>) -> Result<Value, WriteError> {
    let mut tx = pool.begin().await?;
    ensure_app_source(&mut tx).await?;

    let id: String = sqlx::query_scalar(
        "WITH n AS (SELECT gen_random_uuid() AS id) \
         INSERT INTO lifeui.events (id, iri, class_iri, name, description, occurred_at, occurred_on, started_at, ended_at, \
                                    status, sensitivity, source_id, confidence, payload) \
         SELECT n.id, $1 || n.id::text, $2, $3, $4, $5::timestamptz, $6::date, $7::timestamptz, $8::timestamptz, \
                $9, $10, (SELECT id FROM lifeui.sources WHERE iri = $11), $12::numeric, $13::jsonb FROM n \
         RETURNING id::text",
    )
    .bind(format!("{ID_BASE}event-"))
    .bind(&event.class_iri)
    .bind(&event.name)
    .bind(event.description.as_deref())
    .bind(event.occurred_at.as_deref())
    .bind(event.occurred_on.as_deref())
    .bind(event.started_at.as_deref())
    .bind(event.ended_at.as_deref())
    .bind(&event.status)
    .bind(&event.sensitivity)
    .bind(APP_SOURCE_IRI)
    .bind(event.confidence.to_string())
    .bind(event.payload.to_string())
    .persistent(false)
    .fetch_one(&mut *tx)
    .await?;

    if let Some(old) = &event.supersedes {
        let replaced = sqlx::query("UPDATE lifeui.events SET superseded_by = $1::uuid WHERE id = $2::uuid AND superseded_by IS NULL")
            .bind(&id)
            .bind(old)
            .persistent(false)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if replaced != 1 {
            tx.rollback().await?;
            return Err(WriteError::SupersedeConflict);
        }
    }

    if let Some(item) = &event.from_proposal {
        // the agent's proposal becomes this event: mark it added and close the inbox item, in the same transaction
        let marked = sqlx::query(
            "UPDATE lifeui.agent_inbox SET proposal_status = 'added', proposal_event_id = $1::uuid, status = 'done', updated_at = now() \
             WHERE id = $2::uuid AND proposal_status = 'pending'",
        )
        .bind(&id)
        .bind(item)
        .persistent(false)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if marked != 1 {
            tx.rollback().await?;
            return Err(WriteError::ProposalConflict);
        }
        store::audit_in(&mut tx, "agent_inbox_update", Some(item), ip, serde_json::json!({ "status": "done", "proposal": "added", "event": id }))
            .await?;
    }

    let class = event.class_iri.rsplit('#').next().unwrap_or("Event");
    store::audit_in(&mut tx, "event_create", Some(&id), ip, serde_json::json!({ "class": class, "supersedes": event.supersedes })).await?;

    let text: String = sqlx::query_scalar(&format!("SELECT {EVENT_JSON}::text FROM lifeui.events e WHERE e.id = $1::uuid"))
        .bind(&id)
        .persistent(false)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(json(text))
}

pub async fn get_event(pool: &PgPool, id: &str) -> Result<Option<Value>, sqlx::Error> {
    let text: Option<String> = sqlx::query_scalar(&format!("SELECT {EVENT_JSON}::text FROM lifeui.events e WHERE e.id = $1::uuid"))
        .bind(id)
        .persistent(false)
        .fetch_optional(pool)
        .await?;
    Ok(text.map(json))
}

/// The timeline: newest first by when it happened (occurred_at, else started_at, else occurred_on).
pub async fn list_events(pool: &PgPool, filter: &EventFilter) -> Result<Value, sqlx::Error> {
    let sql = format!(
        "SELECT coalesce(jsonb_agg(t.j ORDER BY t.sort_key DESC, t.id DESC), '[]'::jsonb)::text FROM ( \
           SELECT {EVENT_JSON} AS j, {EVENT_SORT_KEY} AS sort_key, e.id \
           FROM lifeui.events e \
           WHERE ($1::text IS NULL OR e.class_iri = $1) \
             AND ($2::text IS NULL OR e.status = $2) \
             AND ($3::timestamptz IS NULL OR {EVENT_SORT_KEY} >= $3::timestamptz) \
             AND ($4::timestamptz IS NULL OR {EVENT_SORT_KEY} < $4::timestamptz) \
             AND ($5::boolean OR e.superseded_by IS NULL) \
           ORDER BY {EVENT_SORT_KEY} DESC, e.id DESC LIMIT $6) t"
    );
    let text: String = sqlx::query_scalar(&sql)
        .bind(filter.class.as_ref().map(|c| format!("{ONTOLOGY_BASE}{c}")))
        .bind(filter.status.as_deref())
        .bind(filter.from.as_deref())
        .bind(filter.to.as_deref())
        .bind(filter.include_superseded)
        .bind(filter.limit)
        .persistent(false)
        .fetch_one(pool)
        .await?;
    Ok(json(text))
}

// ------------------------------------------------------------------------------------ entities (read-only)
// Everything in the knowledge base that is not an event (documents, projects, tasks, critical-information pointers,
// courses, people, tools...). Loaded by tools/import_knowledge.py; the app only reads it for now.

pub struct EntityFilter {
    pub class: Option<String>, // short class name (letters and digits only), compared with the ontology IRI
    pub q: Option<String>,     // part of the name, case-insensitive
    pub limit: i64,
}

/// Escapes LIKE wildcards so a user's text is matched literally.
fn like_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// `{"entities": [...], "classes": [{"class", "count"}]}`: the list (sorted by class, then name) and the class counts.
pub async fn list_entities(pool: &PgPool, filter: &EntityFilter) -> Result<Value, sqlx::Error> {
    let sql = "SELECT jsonb_build_object( \
        'entities', coalesce((SELECT jsonb_agg(s.x ORDER BY s.class_iri, s.name) FROM ( \
            SELECT jsonb_build_object('iri', e.iri, 'class', regexp_replace(e.class_iri, '^.*#', ''), 'name', e.name, \
                   'status', e.status, 'sensitivity', e.sensitivity, \
                   'due', e.props->'dueDate', 'priority', e.props->'priority', 'description', e.props->'description') AS x, e.class_iri, e.name \
            FROM lifeui.entities e \
            WHERE ($1::text IS NULL OR e.class_iri = $1) AND ($2::text IS NULL OR e.name ILIKE '%' || $2 || '%' ESCAPE '\\') \
            ORDER BY e.class_iri, e.name LIMIT $3) s), '[]'::jsonb), \
        'classes', (SELECT coalesce(jsonb_agg(jsonb_build_object('class', k.c, 'count', k.n) ORDER BY k.n DESC, k.c), '[]'::jsonb) \
                    FROM (SELECT regexp_replace(class_iri, '^.*#', '') AS c, count(*) AS n FROM lifeui.entities GROUP BY 1) k) \
      )::text";
    let class_iri = filter.class.as_ref().map(|c| format!("{ONTOLOGY_BASE}{c}"));
    let q = filter.q.as_ref().map(|q| like_escape(q));
    let text: String = sqlx::query_scalar(sql)
        .bind(class_iri)
        .bind(q)
        .bind(filter.limit)
        .persistent(false)
        .fetch_one(pool)
        .await?;
    Ok(json(text))
}

/// One entity with its properties and its relations in both directions (names resolved across all tables).
pub async fn get_entity(pool: &PgPool, iri: &str) -> Result<Option<Value>, sqlx::Error> {
    let sql = "WITH names AS ( \
          SELECT iri, name, regexp_replace(class_iri, '^.*#', '') AS class, 'entity' AS kind FROM lifeui.entities \
          UNION ALL SELECT iri, name, regexp_replace(class_iri, '^.*#', ''), 'event' FROM lifeui.events \
          UNION ALL SELECT iri, label, 'Source', 'source' FROM lifeui.sources) \
        SELECT jsonb_build_object( \
          'entity', jsonb_build_object('iri', e.iri, 'class', regexp_replace(e.class_iri, '^.*#', ''), 'name', e.name, 'status', e.status, \
                    'sensitivity', e.sensitivity, 'confidence', e.confidence::float8, 'props', e.props, \
                    'source', (SELECT s.label FROM lifeui.sources s WHERE s.id = e.source_id)), \
          'links_out', (SELECT coalesce(jsonb_agg(jsonb_build_object('predicate', l.predicate, 'iri', l.object_iri, 'name', n.name, 'class', n.class, 'kind', n.kind) \
                        ORDER BY l.predicate, n.name), '[]'::jsonb) \
                        FROM lifeui.entity_links l LEFT JOIN names n ON n.iri = l.object_iri WHERE l.subject_iri = e.iri), \
          'links_in', (SELECT coalesce(jsonb_agg(jsonb_build_object('predicate', l.predicate, 'iri', l.subject_iri, 'name', n.name, 'class', n.class, 'kind', n.kind) \
                        ORDER BY l.predicate, n.name), '[]'::jsonb) \
                        FROM lifeui.entity_links l LEFT JOIN names n ON n.iri = l.subject_iri WHERE l.object_iri = e.iri) \
        )::text FROM lifeui.entities e WHERE e.iri = $1";
    let text: Option<String> = sqlx::query_scalar(sql).bind(iri).persistent(false).fetch_optional(pool).await?;
    Ok(text.map(json))
}

#[cfg(test)]
mod entity_tests {
    use super::like_escape;

    #[test]
    fn like_wildcards_are_matched_literally() {
        assert_eq!(like_escape("100%_done\\"), "100\\%\\_done\\\\");
        assert_eq!(like_escape("plain"), "plain");
    }
}
