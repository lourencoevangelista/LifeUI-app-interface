//! The News tab: the daily news briefing about the owner's interests that the home server builds (tools/news/) and publishes
//! through `POST /api/runner/news` with a runner token, and the owner's read / save / dismiss marks.
//!
//! Like the agent tables these rows are operational data, not knowledge: publishing a briefing for a date again
//! REPLACES that briefing (the same row is updated, items no longer in it are removed), while the owner's marks
//! on an item survive (an item is matched by its URL, which is unique). The audit log records each publish and
//! each mark, never titles or text.
//!
//! Extras (Phase 6): a briefing may carry `extras` = {calendar, digest, feedback}. A WEEKLY briefing has no items of
//! its own: its `extras.digest.urls` point at daily items (so read / save marks are shared between the two views).
//! Saving an item also files it as a LifeUI Task ("reading list", origin 'app'); unsaving removes that task.
//! GET /api/news reports `today` (inbox count, LifeUI events today, and `missing` once it is past 09:45 local time
//! without a daily briefing; that is also written to the audit log, once per day).
//!
//! The "Stay awake tonight" toggle is a date in lifeui.agent_settings: the home server reads it through
//! `GET /api/runner` before its 21:00 shutdown and skips the shutdown on that date (local time).
//!
//! "Local time" is the owner's time zone: the LIFEUI_TIMEZONE environment variable (an IANA name such as
//! America/New_York; default UTC), see `time_zone()`.

use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;

use crate::domain::{ID_BASE, ONTOLOGY_BASE};
use crate::store;

/// Mirror NEWS_BRIEFING_TYPES / NEWS_TAGS in db/src/schema.ts (a test fails on drift).
pub const BRIEFING_TYPES: [&str; 2] = ["daily", "weekly"];
pub const TAGS: [&str; 5] = ["ai_breakthrough", "release", "research", "focus_relevant", "industry"];
/// What the owner can do to an item.
pub const ITEM_ACTIONS: [&str; 6] = ["read", "unread", "save", "unsave", "dismiss", "undismiss"];

pub const MAX_ITEMS: usize = 30;
pub const INTRO_MAX: usize = 2000;
pub const TITLE_MAX: usize = 300;
pub const SUMMARY_MAX: usize = 1000;
pub const WHY_MAX: usize = 500;
pub const URL_MAX: usize = 2000;
pub const SOURCE_MAX: usize = 120;
pub const MODEL_MAX: usize = 200;
/// The weather, server status and stats blocks are each at most this big (as JSON text).
pub const BLOCK_MAX_BYTES: usize = 48 * 1024;
/// A weekly digest lists at most this many items, highlights and trends.
pub const DIGEST_MAX_ITEMS: usize = 15;
pub const DIGEST_MAX_HIGHLIGHTS: usize = 5;
pub const DIGEST_MAX_TRENDS: usize = 3;
/// After this local time a missing daily briefing means the morning task did not run (or failed).
pub const MISSING_AFTER: &str = "09:45";
/// Whole request body for a publish.
pub const PUBLISH_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Deserialize)]
pub struct ItemInput {
    pub url: String,
    pub title: String,
    pub source: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    pub importance: i64,
    pub focus_relevance: i64,
    pub summary: String,
    #[serde(default)]
    pub why_it_matters: Option<String>,
    #[serde(default)]
    pub points: Option<i64>,
}

/// The briefing JSON written by tools/news/news.py (+ server_status from the morning task). Unknown fields
/// (stats details, per-source report, item ids) are ignored.
#[derive(Debug, Deserialize)]
pub struct BriefingInput {
    pub date: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub intro: String,
    #[serde(default)]
    pub weather: Option<Value>,
    #[serde(default)]
    pub server_status: Option<Value>,
    #[serde(default)]
    pub generated_by_model: String,
    pub generated_at: String,
    #[serde(default)]
    pub stats: Option<Value>,
    #[serde(default)]
    pub extras: Option<Value>,
    #[serde(default)]
    pub items: Vec<ItemInput>,
}

#[derive(Debug)]
pub struct ValidItem {
    pub url: String,
    pub title: String,
    pub source: String,
    pub published_at: Option<String>,
    pub tags: Vec<String>,
    pub topics: Vec<String>,
    pub importance: i16,
    pub focus_relevance: i16,
    pub summary: String,
    pub why_it_matters: String,
    pub points: Option<i32>,
}

#[derive(Debug)]
pub struct ValidBriefing {
    pub date: String,
    pub kind: String,
    pub intro: String,
    pub weather: Option<Value>,
    pub server_status: Option<Value>,
    pub generated_by_model: String,
    pub generated_at: String,
    pub stats: Value,
    pub extras: Value,
    pub items: Vec<ValidItem>,
}

fn is_date(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
        && time::Date::parse(text, &time::format_description::well_known::Iso8601::DEFAULT).is_ok()
}

fn is_instant(text: &str) -> bool {
    time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).is_ok()
}

fn is_web_url(text: &str) -> bool {
    (text.starts_with("https://") || text.starts_with("http://"))
        && text.len() <= URL_MAX
        && text.len() > 10
        && !text.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn bounded(field: &str, text: &str, min: usize, max: usize, problems: &mut Vec<String>) -> String {
    let text = text.trim();
    let n = text.chars().count();
    if n < min || n > max {
        problems.push(format!("{field} must have between {min} and {max} characters."));
    }
    text.to_string()
}

fn block(field: &str, value: Option<Value>, problems: &mut Vec<String>) -> Option<Value> {
    let value = value.filter(|v| !v.is_null())?;
    if !value.is_object() {
        problems.push(format!("{field} must be an object."));
    } else if value.to_string().len() > BLOCK_MAX_BYTES {
        problems.push(format!("{field} is too large (maximum {} KB).", BLOCK_MAX_BYTES / 1024));
    }
    Some(value)
}

pub fn validate_briefing(input: BriefingInput) -> Result<ValidBriefing, Vec<String>> {
    let mut problems = Vec::new();
    if !is_date(&input.date) {
        problems.push("date must be YYYY-MM-DD.".into());
    }
    let kind = input.kind.unwrap_or_else(|| "daily".into());
    if !BRIEFING_TYPES.contains(&kind.as_str()) {
        problems.push(format!("type must be one of: {}.", BRIEFING_TYPES.join(", ")));
    }
    let intro = bounded("intro", &input.intro, 0, INTRO_MAX, &mut problems);
    let generated_by_model = bounded("generated_by_model", &input.generated_by_model, 0, MODEL_MAX, &mut problems);
    if !is_instant(&input.generated_at) {
        problems.push("generated_at must be an RFC 3339 timestamp.".into());
    }
    let weather = block("weather", input.weather, &mut problems);
    let server_status = block("server_status", input.server_status, &mut problems);
    let stats = block("stats", input.stats, &mut problems).unwrap_or_else(|| serde_json::json!({}));
    let extras = block("extras", input.extras, &mut problems).unwrap_or_else(|| serde_json::json!({}));
    check_extras(&kind, &extras, input.items.len(), &mut problems);
    if input.items.len() > MAX_ITEMS {
        problems.push(format!("a briefing has at most {MAX_ITEMS} items."));
    }
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (n, it) in input.items.into_iter().take(MAX_ITEMS).enumerate() {
        let at = format!("items[{n}]");
        let url = it.url.trim().to_string();
        if !is_web_url(&url) {
            problems.push(format!("{at}.url must be an http(s) address without spaces (at most {URL_MAX} characters)."));
        } else if !seen.insert(url.clone()) {
            problems.push(format!("{at}.url appears twice in this briefing."));
        }
        let title = bounded(&format!("{at}.title"), &it.title, 1, TITLE_MAX, &mut problems);
        let source = bounded(&format!("{at}.source"), &it.source, 1, SOURCE_MAX, &mut problems);
        let summary = bounded(&format!("{at}.summary"), &it.summary, 1, SUMMARY_MAX, &mut problems);
        let why = bounded(&format!("{at}.why_it_matters"), it.why_it_matters.as_deref().unwrap_or(""), 0, WHY_MAX, &mut problems);
        let published_at = it.published_at.filter(|s| !s.trim().is_empty());
        if published_at.as_deref().is_some_and(|p| !is_instant(p)) {
            problems.push(format!("{at}.published_at must be an RFC 3339 timestamp."));
        }
        for (name, score) in [("importance", it.importance), ("focus_relevance", it.focus_relevance)] {
            if !(0..=10).contains(&score) {
                problems.push(format!("{at}.{name} must be a whole number from 0 to 10."));
            }
        }
        if let Some(bad) = it.tags.iter().find(|t| !TAGS.contains(&t.as_str())) {
            problems.push(format!("{at}.tags: '{bad}' is not one of {}.", TAGS.join(", ")));
        }
        let mut tags = it.tags;
        tags.dedup();
        let topics: Vec<String> = it.topics.into_iter().filter(|t| t.len() <= 40).take(12).collect();
        items.push(ValidItem {
            url,
            title,
            source,
            published_at,
            tags,
            topics,
            importance: it.importance.clamp(0, 10) as i16,
            focus_relevance: it.focus_relevance.clamp(0, 10) as i16,
            summary,
            why_it_matters: why,
            points: it.points.filter(|p| (0..=10_000_000).contains(p)).map(|p| p as i32),
        });
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(ValidBriefing {
        date: input.date,
        kind,
        intro,
        weather,
        server_status,
        generated_by_model,
        generated_at: input.generated_at,
        stats,
        extras,
        items,
    })
}

fn strings(value: &Value) -> Option<Vec<&str>> {
    value.as_array()?.iter().map(Value::as_str).collect()
}

/// calendar: a list; feedback: an object; digest (weekly only, and required there): {urls, highlights, trends}.
fn check_extras(kind: &str, extras: &Value, item_count: usize, problems: &mut Vec<String>) {
    if let Some(cal) = extras.get("calendar").filter(|c| !c.is_null()) {
        if !cal.as_array().is_some_and(|a| a.len() <= 40 && a.iter().all(Value::is_object)) {
            problems.push("extras.calendar must be a list of at most 40 events.".into());
        }
    }
    if extras.get("feedback").is_some_and(|f| !f.is_null() && !f.is_object()) {
        problems.push("extras.feedback must be an object.".into());
    }
    let digest = extras.get("digest").filter(|d| !d.is_null());
    if kind != "weekly" {
        if digest.is_some() {
            problems.push("extras.digest belongs to a weekly briefing.".into());
        }
        return;
    }
    if item_count != 0 {
        problems.push("a weekly briefing has no items of its own: list them in extras.digest.urls.".into());
    }
    let Some(digest) = digest else {
        problems.push("a weekly briefing needs extras.digest.".into());
        return;
    };
    let urls = strings(&digest["urls"]).unwrap_or_default();
    if urls.is_empty() || urls.len() > DIGEST_MAX_ITEMS || !urls.iter().all(|u| is_web_url(u)) {
        problems.push(format!("extras.digest.urls must list 1 to {DIGEST_MAX_ITEMS} item addresses."));
    }
    let highlights = strings(&digest["highlights"]).unwrap_or_default();
    if highlights.len() > DIGEST_MAX_HIGHLIGHTS || !highlights.iter().all(|h| urls.contains(h)) {
        problems.push(format!("extras.digest.highlights must be at most {DIGEST_MAX_HIGHLIGHTS} of the digest's urls."));
    }
    let trends_ok = digest["trends"].as_array().is_some_and(|t| {
        t.len() <= DIGEST_MAX_TRENDS
            && t.iter().all(|x| {
                let title = x["title"].as_str().map_or(0, |s| s.chars().count());
                let text = x["text"].as_str().map_or(0, |s| s.chars().count());
                (1..=120).contains(&title) && text <= 500
            })
    });
    if !trends_ok {
        problems.push(format!("extras.digest.trends must be at most {DIGEST_MAX_TRENDS} {{title, text}} objects."));
    }
}

/// What POST /api/news accepts (owner, logged in).
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NewsWrite {
    /// read / unread / save / unsave / dismiss / undismiss one item
    Item { id: String, action: String },
    /// "Stay awake tonight": keep the home server on tonight (true) or let it shut down as usual (false)
    StayAwake { on: bool },
}

pub fn validate_item_action(id: &str, action: &str) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    if !crate::domain::is_uuid(id) {
        problems.push("id must be a UUID.".into());
    }
    if !ITEM_ACTIONS.contains(&action) {
        problems.push(format!("action must be one of: {}.", ITEM_ACTIONS.join(", ")));
    }
    if problems.is_empty() { Ok(()) } else { Err(problems) }
}

// ------------------------------------------------------------------------------------ SQL

fn json(text: String) -> Value {
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

const ITEM_JSON: &str = "jsonb_build_object('id', i.id::text, 'url', i.url, 'title', i.title, 'source', i.source, \
    'published_at', i.published_at, 'tags', i.tags, 'topics', i.topics, 'importance', i.importance, \
    'focus_relevance', i.focus_relevance, 'summary', i.summary, 'why_it_matters', i.why_it_matters, 'points', i.points, \
    'read_at', i.read_at, 'saved_at', i.saved_at, 'dismissed_at', i.dismissed_at)";

/// Insert or replace the briefing for its date and type, in one transaction. Returns {id, date, items}.
pub async fn publish(pool: &PgPool, b: &ValidBriefing, token_id: &str) -> Result<Value, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let id: String = sqlx::query_scalar(
        "INSERT INTO lifeui.news_briefings (date, type, intro, weather, server_status, generated_by_model, generated_at, stats, extras) \
         VALUES ($1::date, $2, $3, $4::jsonb, $5::jsonb, $6, $7::timestamptz, $8::jsonb, $9::jsonb) \
         ON CONFLICT (date, type) DO UPDATE SET intro = EXCLUDED.intro, weather = EXCLUDED.weather, \
           server_status = EXCLUDED.server_status, generated_by_model = EXCLUDED.generated_by_model, \
           generated_at = EXCLUDED.generated_at, stats = EXCLUDED.stats, extras = EXCLUDED.extras, published_at = now() \
         RETURNING id::text",
    )
    .bind(&b.date)
    .bind(&b.kind)
    .bind(&b.intro)
    .bind(b.weather.as_ref().map(Value::to_string))
    .bind(b.server_status.as_ref().map(Value::to_string))
    .bind(&b.generated_by_model)
    .bind(&b.generated_at)
    .bind(b.stats.to_string())
    .bind(b.extras.to_string())
    .persistent(false)
    .fetch_one(&mut *tx)
    .await?;
    let urls: Vec<String> = b.items.iter().map(|i| i.url.clone()).collect();
    sqlx::query("DELETE FROM lifeui.news_items WHERE briefing_id = $1::uuid AND NOT (url = ANY($2))")
        .bind(&id)
        .bind(&urls)
        .persistent(false)
        .execute(&mut *tx)
        .await?;
    for (position, it) in b.items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO lifeui.news_items (briefing_id, position, url, title, source, published_at, tags, topics, importance, \
               focus_relevance, summary, why_it_matters, points) \
             VALUES ($1::uuid, $2, $3, $4, $5, $6::timestamptz, $7::jsonb, $8::jsonb, $9, $10, $11, $12, $13) \
             ON CONFLICT (url) DO UPDATE SET briefing_id = EXCLUDED.briefing_id, position = EXCLUDED.position, \
               title = EXCLUDED.title, source = EXCLUDED.source, published_at = EXCLUDED.published_at, tags = EXCLUDED.tags, \
               topics = EXCLUDED.topics, importance = EXCLUDED.importance, focus_relevance = EXCLUDED.focus_relevance, \
               summary = EXCLUDED.summary, why_it_matters = EXCLUDED.why_it_matters, points = EXCLUDED.points",
        )
        .bind(&id)
        .bind(position as i16)
        .bind(&it.url)
        .bind(&it.title)
        .bind(&it.source)
        .bind(&it.published_at)
        .bind(serde_json::json!(it.tags).to_string())
        .bind(serde_json::json!(it.topics).to_string())
        .bind(it.importance)
        .bind(it.focus_relevance)
        .bind(&it.summary)
        .bind(&it.why_it_matters)
        .bind(it.points)
        .persistent(false)
        .execute(&mut *tx)
        .await?;
    }
    store::audit_in(
        &mut tx,
        "news_publish",
        Some(&id),
        None,
        serde_json::json!({ "date": b.date, "type": b.kind, "items": b.items.len(), "token": token_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(serde_json::json!({ "id": id, "date": b.date, "type": b.kind, "items": b.items.len() }))
}

/// One briefing with its items (dismissed ones included, flagged), or null. Without a date: the newest one.
/// A weekly briefing's items are the daily items its digest points at, in the digest's order.
pub async fn briefing(pool: &PgPool, date: Option<&str>, kind: &str) -> Result<Value, sqlx::Error> {
    let text: Option<String> = sqlx::query_scalar(&format!(
        "SELECT jsonb_build_object('id', b.id::text, 'date', b.date, 'type', b.type, 'intro', b.intro, 'weather', b.weather, \
           'server_status', b.server_status, 'generated_by_model', b.generated_by_model, 'generated_at', b.generated_at, \
           'published_at', b.published_at, 'stats', b.stats, 'extras', b.extras, \
           'items', CASE WHEN b.type = 'weekly' THEN \
             (SELECT coalesce(jsonb_agg({ITEM_JSON} ORDER BY p.n), '[]'::jsonb) \
                FROM jsonb_array_elements_text(coalesce(b.extras->'digest'->'urls', '[]'::jsonb)) WITH ORDINALITY p(url, n) \
                JOIN lifeui.news_items i ON i.url = p.url) \
           ELSE \
             (SELECT coalesce(jsonb_agg({ITEM_JSON} ORDER BY i.position), '[]'::jsonb) FROM lifeui.news_items i WHERE i.briefing_id = b.id) \
           END \
         )::text FROM lifeui.news_briefings b \
         WHERE b.type = $2 AND ($1::date IS NULL OR b.date = $1::date) ORDER BY b.date DESC LIMIT 1"
    ))
    .bind(date)
    .bind(kind)
    .persistent(false)
    .fetch_optional(pool)
    .await?;
    Ok(text.map(json).unwrap_or(Value::Null))
}

/// The dates that have a briefing of this type, newest first (for the date picker).
pub async fn dates(pool: &PgPool, kind: &str) -> Result<Value, sqlx::Error> {
    let text: String = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(d ORDER BY d DESC), '[]'::jsonb)::text FROM \
           (SELECT date AS d FROM lifeui.news_briefings WHERE type = $1 ORDER BY date DESC LIMIT 120) x",
    )
    .bind(kind)
    .persistent(false)
    .fetch_one(pool)
    .await?;
    Ok(json(text))
}

/// Apply one owner action to an item. Ok(None): no such item.
pub async fn item_action(pool: &PgPool, id: &str, action: &str, ip: Option<&str>) -> Result<Option<Value>, sqlx::Error> {
    let set = match action {
        "read" => "read_at = coalesce(read_at, now())",
        "unread" => "read_at = NULL",
        "save" => "saved_at = coalesce(saved_at, now()), read_at = coalesce(read_at, now())",
        "unsave" => "saved_at = NULL",
        "dismiss" => "dismissed_at = coalesce(dismissed_at, now())",
        _ => "dismissed_at = NULL", // undismiss (validated before)
    };
    let mut tx = pool.begin().await?;
    let text: Option<String> = sqlx::query_scalar(&format!(
        "WITH i AS (UPDATE lifeui.news_items SET {set} WHERE id = $1::uuid RETURNING *) SELECT {ITEM_JSON}::text FROM i"
    ))
    .bind(id)
    .persistent(false)
    .fetch_optional(&mut *tx)
    .await?;
    if text.is_some() {
        match action {
            "save" => save_task(&mut tx, id).await?,
            "unsave" => remove_task(&mut tx, id).await?,
            _ => {}
        }
        store::audit_in(&mut tx, "news_item_update", Some(id), ip, serde_json::json!({ "action": action })).await?;
    }
    tx.commit().await?;
    Ok(text.map(json))
}

/// The reading-list task of a news item: one per URL (saving the same story from the daily and weekly views is one task).
const TASK_IRI: &str = "$2 || 'news-' || md5(i.url)";

/// Saved item -> a LifeUI Task (status Planned, origin 'app', so the knowledge import never touches it).
async fn save_task(tx: &mut sqlx::PgConnection, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(&format!(
        "INSERT INTO lifeui.entities (iri, class_iri, name, status, sensitivity, props, origin) \
         SELECT {TASK_IRI}, $3, left('Read: ' || i.title, 300), 'Planned', 'Personal', \
           jsonb_build_object('description', i.summary, 'url', i.url, 'source', i.source, 'list', 'reading', 'newsItemId', i.id::text), 'app' \
         FROM lifeui.news_items i WHERE i.id = $1::uuid \
         ON CONFLICT (iri) DO NOTHING"
    ))
    .bind(id)
    .bind(ID_BASE)
    .bind(format!("{ONTOLOGY_BASE}Task"))
    .persistent(false)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Unsaved item -> its reading-list task goes away (only the one the app created).
async fn remove_task(tx: &mut sqlx::PgConnection, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(&format!(
        "DELETE FROM lifeui.entities e USING lifeui.news_items i \
         WHERE i.id = $1::uuid AND e.iri = {TASK_IRI} AND e.origin = 'app'"
    ))
    .bind(id)
    .bind(ID_BASE)
    .persistent(false)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// For the Today card: {date, inbox_open, events: [{name, class, time (HH:MM local, null = all day)}], missing}.
/// `missing` = it is past 09:45 local time and today has no daily briefing; the first time a day is found missing it is
/// written to the audit log.
pub async fn today(pool: &PgPool, ip: Option<&str>) -> Result<Value, sqlx::Error> {
    let (tz, local_today) = (time_zone(), local_today());
    let text: String = sqlx::query_scalar(&format!(
        "WITH d AS (SELECT {local_today} AS day, (now() AT TIME ZONE '{tz}')::time >= $1::time AS late) \
         SELECT jsonb_build_object( \
           'date', d.day, \
           'inbox_open', (SELECT count(*) FROM lifeui.agent_inbox WHERE status IN ('new', 'picked_up', 'needs_answer')), \
           'events', (SELECT coalesce(jsonb_agg(jsonb_build_object('name', e.name, 'class', regexp_replace(e.class_iri, '^.*#', ''), \
                        'time', to_char(coalesce(e.occurred_at, e.started_at) AT TIME ZONE '{tz}', 'HH24:MI')) \
                        ORDER BY coalesce(e.occurred_at, e.started_at, e.occurred_on::timestamptz)), '[]'::jsonb) \
                      FROM lifeui.events e \
                      WHERE e.superseded_by IS NULL AND e.sensitivity IN ('Public', 'Personal') \
                        AND coalesce((coalesce(e.occurred_at, e.started_at) AT TIME ZONE '{tz}')::date, e.occurred_on) = d.day), \
           'missing', d.late AND NOT EXISTS (SELECT 1 FROM lifeui.news_briefings b WHERE b.type = 'daily' AND b.date = d.day) \
         )::text FROM d"
    ))
    .bind(MISSING_AFTER)
    .persistent(false)
    .fetch_one(pool)
    .await?;
    let value = json(text);
    if value["missing"].as_bool() == Some(true) {
        let day = value["date"].as_str().unwrap_or_default().to_string();
        let logged: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lifeui.audit_log WHERE action = 'news_missing' AND target = $1)")
            .bind(&day)
            .persistent(false)
            .fetch_one(pool)
            .await?;
        if !logged {
            store::audit(pool, "news_missing", Some(&day), ip, serde_json::json!({ "after": MISSING_AFTER })).await;
        }
    }
    Ok(value)
}

/// For the home server's weekly digest and monthly feedback job (runner token): the daily items of the last `days`
/// days with the owner's marks. No intro, weather or server status.
pub async fn signals(pool: &PgPool, days: i32) -> Result<Value, sqlx::Error> {
    let local_today = local_today();
    let text: String = sqlx::query_scalar(&format!(
        "SELECT coalesce(jsonb_agg(jsonb_build_object('date', b.date, 'url', i.url, 'title', i.title, 'source', i.source, \
           'tags', i.tags, 'topics', i.topics, 'importance', i.importance, 'focus_relevance', i.focus_relevance, \
           'summary', i.summary, 'why_it_matters', i.why_it_matters, 'points', i.points, \
           'read', i.read_at IS NOT NULL, 'saved', i.saved_at IS NOT NULL, 'dismissed', i.dismissed_at IS NOT NULL) \
           ORDER BY b.date, i.position), '[]'::jsonb)::text \
         FROM lifeui.news_items i JOIN lifeui.news_briefings b ON b.id = i.briefing_id \
         WHERE b.type = 'daily' AND b.date > {local_today} - $1::int"
    ))
    .bind(days)
    .persistent(false)
    .fetch_one(pool)
    .await?;
    Ok(json(text))
}

/// The owner's time zone: LIFEUI_TIMEZONE (an IANA name such as Europe/Berlin), UTC when unset. It is written into
/// SQL text, so anything but a plain zone name (letters, digits, `/`, `_`, `+`, `-`) is ignored and UTC is used.
pub fn time_zone() -> String {
    clean_time_zone(std::env::var("LIFEUI_TIMEZONE").ok().as_deref())
}

pub fn clean_time_zone(value: Option<&str>) -> String {
    match value.map(str::trim) {
        Some(tz)
            if !tz.is_empty()
                && tz.len() <= 64
                && tz.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '+' | '-')) =>
        {
            tz.to_string()
        }
        _ => "UTC".to_string(),
    }
}

/// Today's date in the owner's time zone, as the database sees it.
fn local_today() -> String {
    format!("(now() AT TIME ZONE '{}')::date", time_zone())
}

/// {"stay_awake_on": "YYYY-MM-DD" or null, "tonight": bool}
pub async fn stay_awake(pool: &PgPool) -> Result<Value, sqlx::Error> {
    let local_today = local_today();
    let text: String = sqlx::query_scalar(&format!(
        "SELECT jsonb_build_object('stay_awake_on', (SELECT stay_awake_on FROM lifeui.agent_settings WHERE id = 1), \
           'tonight', coalesce((SELECT stay_awake_on = {local_today} FROM lifeui.agent_settings WHERE id = 1), false))::text"
    ))
    .persistent(false)
    .fetch_one(pool)
    .await?;
    Ok(json(text))
}

/// Turn "Stay awake tonight" on (today's local date) or off.
pub async fn set_stay_awake(pool: &PgPool, on: bool, ip: Option<&str>) -> Result<Value, sqlx::Error> {
    let local_today = local_today();
    let mut tx = pool.begin().await?;
    sqlx::query(&format!(
        "INSERT INTO lifeui.agent_settings (id, stay_awake_on) VALUES (1, CASE WHEN $1 THEN {local_today} END) \
         ON CONFLICT (id) DO UPDATE SET stay_awake_on = EXCLUDED.stay_awake_on, updated_at = now()"
    ))
    .bind(on)
    .persistent(false)
    .execute(&mut *tx)
    .await?;
    store::audit_in(&mut tx, "agent_stay_awake", None, ip, serde_json::json!({ "on": on })).await?;
    tx.commit().await?;
    stay_awake(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item() -> Value {
        json!({"url": "https://deepmind.google/blog/gemini-4", "title": "Introducing Gemini 4 Neon", "source": "Google DeepMind",
               "published_at": "2026-11-19T10:00:00+00:00", "tags": ["release"], "topics": ["agent_models"], "importance": 9,
               "focus_relevance": 4, "summary": "One. Two.", "why_it_matters": "Because.", "points": 900, "id": "ignored"})
    }

    fn briefing(items: Vec<Value>) -> Value {
        json!({"date": "2026-11-19", "type": "daily", "intro": "3 things that matter today\n- a", "weather": {"summary": "Faro"},
               "server_status": null, "generated_by_model": "ollama qwen3:8b", "generated_at": "2026-11-19T09:05:00+00:00",
               "stats": {"fetched": 10}, "sources": {"openai": "ok 3"}, "items": items})
    }

    fn check(value: Value) -> Result<ValidBriefing, Vec<String>> {
        validate_briefing(serde_json::from_value(value).expect("shape"))
    }

    #[test]
    fn time_zone_is_a_plain_name_or_utc() {
        assert_eq!(clean_time_zone(Some("America/New_York")), "America/New_York");
        assert_eq!(clean_time_zone(Some(" Etc/GMT+3 ")), "Etc/GMT+3");
        assert_eq!(clean_time_zone(None), "UTC");
        assert_eq!(clean_time_zone(Some("")), "UTC");
        assert_eq!(clean_time_zone(Some("UTC'; DROP TABLE x; --")), "UTC");
    }

    #[test]
    fn a_real_briefing_is_accepted_and_unknown_fields_ignored() {
        let b = check(briefing(vec![item()])).unwrap();
        assert_eq!((b.date.as_str(), b.kind.as_str(), b.items.len()), ("2026-11-19", "daily", 1));
        assert_eq!(b.items[0].points, Some(900));
        assert!(b.server_status.is_none());
    }

    #[test]
    fn bad_items_are_all_reported() {
        let mut bad = item();
        bad["url"] = json!("javascript:alert(1)");
        bad["importance"] = json!(11);
        bad["tags"] = json!(["gossip"]);
        bad["title"] = json!("");
        let problems = check(briefing(vec![bad])).unwrap_err().join(" | ");
        for expected in ["items[0].url", "items[0].importance", "'gossip'", "items[0].title"] {
            assert!(problems.contains(expected), "{expected} missing from: {problems}");
        }
    }

    #[test]
    fn duplicates_dates_and_limits() {
        assert!(check(briefing(vec![item(), item()])).unwrap_err().join(" ").contains("twice"));
        let mut b = briefing(vec![item()]);
        b["date"] = json!("2026-02-30");
        assert!(check(b).unwrap_err().join(" ").contains("date"));
        let mut b = briefing(vec![item()]);
        b["type"] = json!("hourly");
        assert!(check(b).unwrap_err().join(" ").contains("type"));
        let many: Vec<Value> = (0..31).map(|n| { let mut i = item(); i["url"] = json!(format!("https://example.com/{n}")); i }).collect();
        assert!(check(briefing(many)).unwrap_err().join(" ").contains("at most 30"));
        let mut b = briefing(vec![item()]);
        b["weather"] = json!({"blob": "x".repeat(BLOCK_MAX_BYTES)});
        assert!(check(b).unwrap_err().join(" ").contains("weather is too large"));
        let mut b = briefing(vec![item()]);
        b["weather"] = json!("sunny");
        assert!(check(b).unwrap_err().join(" ").contains("weather must be an object"));
    }

    fn weekly(digest: Value) -> Value {
        let mut b = briefing(vec![]);
        b["type"] = json!("weekly");
        b["extras"] = json!({ "digest": digest });
        b
    }

    #[test]
    fn extras_and_weekly_digests() {
        let digest = json!({"urls": ["https://a.example/1", "https://a.example/2"], "highlights": ["https://a.example/2"],
                            "trends": [{"title": "Agents everywhere", "text": "Three labs shipped agent SDKs."}]});
        let b = check(weekly(digest.clone())).unwrap();
        assert_eq!((b.kind.as_str(), b.items.len()), ("weekly", 0));
        assert_eq!(b.extras["digest"]["urls"][1], "https://a.example/2");

        let mut with_items = weekly(digest.clone());
        with_items["items"] = json!([item()]);
        assert!(check(with_items).unwrap_err().join(" ").contains("no items of its own"));
        let mut b = briefing(vec![]);
        b["type"] = json!("weekly");
        assert!(check(b).unwrap_err().join(" ").contains("needs extras.digest"));
        let mut stray = digest.clone();
        stray["highlights"] = json!(["https://elsewhere.example/x"]);
        assert!(check(weekly(stray)).unwrap_err().join(" ").contains("highlights"));
        let mut many = digest.clone();
        many["trends"] = json!([{"title": "a"}, {"title": "b"}, {"title": "c"}, {"title": "d"}]);
        assert!(check(weekly(many)).unwrap_err().join(" ").contains("trends"));
        let mut bad = digest;
        bad["urls"] = json!(["javascript:alert(1)"]);
        assert!(check(weekly(bad)).unwrap_err().join(" ").contains("urls"));

        let mut daily = briefing(vec![item()]);
        daily["extras"] = json!({"calendar": [{"title": "Dentist", "start": "2026-11-20T15:00:00+00:00"}], "feedback": {"changes": []}});
        assert!(check(daily.clone()).is_ok());
        daily["extras"]["digest"] = json!({"urls": []});
        assert!(check(daily.clone()).unwrap_err().join(" ").contains("weekly"));
        daily["extras"] = json!({"calendar": "today"});
        assert!(check(daily).unwrap_err().join(" ").contains("calendar"));
    }

    #[test]
    fn item_actions() {
        assert!(validate_item_action("123e4567-e89b-12d3-a456-426614174000", "save").is_ok());
        assert!(validate_item_action("x", "save").is_err());
        assert!(validate_item_action("123e4567-e89b-12d3-a456-426614174000", "delete").is_err());
        assert!(serde_json::from_value::<NewsWrite>(json!({"kind": "stay_awake", "on": true})).is_ok());
        assert!(serde_json::from_value::<NewsWrite>(json!({"kind": "stay_awake", "on": true, "extra": 1})).is_err());
    }

    fn ts_list(source: &str, constant: &str) -> Vec<String> {
        let start = source.find(&format!("export const {constant} = [")).unwrap_or_else(|| panic!("{constant} not in schema.ts"));
        let body = &source[start..];
        body[body.find('[').unwrap() + 1..body.find(']').unwrap()]
            .split(',')
            .map(|s| s.trim().trim_matches('"').to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    #[test]
    fn vocabularies_match_the_drizzle_schema() {
        let schema = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/db/src/schema.ts")).unwrap();
        assert_eq!(ts_list(&schema, "NEWS_BRIEFING_TYPES"), BRIEFING_TYPES.map(String::from).to_vec());
        assert_eq!(ts_list(&schema, "NEWS_TAGS"), TAGS.map(String::from).to_vec());
        let tools = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tools/news/news.py")).unwrap();
        assert!(tools.contains(r#"TAGS = ["ai_breakthrough", "release", "research", "focus_relevant", "industry"]"#), "tools/news/news.py TAGS drifted");
    }
}
