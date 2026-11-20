//! The Agent tab: a work queue (inbox: what the owner asks the agent to do), a feed (what the agent says back)
//! and a log of its scheduled runs.
//!
//! These rows are operational data, not knowledge: an inbox item changes status in place (new -> done), unlike
//! events, which are versioned. Nothing here is ever deleted. The audit log records
//! that an inbox item was created or changed, never its text.
//!
//! The scheduled runner (tools/agent_runner.py: morning briefing, midday inbox check, evening overview) talks to
//! /api/runner. It is read-only: it can read a limited snapshot (`runner_context`), answer the inbox and post to the
//! feed, and nothing else. Hand-written feed messages (origin 'manual') remain for testing.

use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;

use crate::store;

/// Mirrors AGENT_* in db/src/schema.ts (a test fails on drift).
pub const SLOTS: [&str; 4] = ["morning", "midday", "evening", "manual"];
pub const INBOX_STATUSES: [&str; 5] = ["new", "picked_up", "done", "needs_answer", "cancelled"];
pub const MESSAGE_KINDS: [&str; 5] = ["briefing", "overview", "question", "proposal", "note"];
pub const RUN_STATUSES: [&str; 4] = ["running", "ok", "failed", "skipped"];
pub const PROPOSAL_STATUSES: [&str; 3] = ["pending", "added", "dismissed"];
/// Statuses the owner may set from the app; the rest belong to the agent.
pub const OWNER_STATUSES: [&str; 3] = ["new", "done", "cancelled"];

pub const INBOX_MAX: usize = 2000;
pub const TITLE_MAX: usize = 200;
pub const MESSAGE_MAX: usize = 8000;
pub const LABEL_MAX: usize = 60;
/// At most this many live (not revoked) runner tokens at once.
pub const MAX_ACTIVE_TOKENS: i64 = 5;

/// What POST /api/agent accepts.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentWrite {
    /// An instruction for the agent.
    Inbox { body: String },
    /// The owner changes the status of one inbox item (done, cancelled, or back to new).
    InboxStatus { id: String, status: String },
    /// A hand-written feed message (testing until the runner exists).
    Message {
        message_kind: String,
        title: String,
        body: String,
        #[serde(default)]
        slot: Option<String>,
    },
    /// The kill switch: false stops the runner API at once.
    Switch { enabled: bool },
    /// A new credential for the runner. The token is shown once, in the answer.
    TokenCreate { label: String },
    TokenRevoke { id: String },
    /// The owner does not want the event the agent proposed for this inbox item (the item is closed as cancelled).
    /// Adding it goes through POST /api/events with "from_proposal", so it gets the same checks as any event.
    ProposalDismiss { id: String },
}

#[derive(Debug, PartialEq)]
pub enum Valid {
    Inbox { body: String },
    InboxStatus { id: String, status: String },
    Message { kind: String, slot: String, title: String, body: String },
    Switch { enabled: bool },
    TokenCreate { label: String },
    TokenRevoke { id: String },
    ProposalDismiss { id: String },
}

fn length_problem(name: &str, value: &str, max: usize, problems: &mut Vec<String>) {
    let n = value.chars().count();
    if n == 0 {
        problems.push(format!("{name} must not be empty."));
    } else if n > max {
        problems.push(format!("{name} must have at most {max} characters (it has {n})."));
    }
}

pub fn validate(input: AgentWrite) -> Result<Valid, Vec<String>> {
    let mut problems = Vec::new();
    let valid = match input {
        AgentWrite::Inbox { body } => {
            let body = body.trim().to_string();
            length_problem("body", &body, INBOX_MAX, &mut problems);
            Valid::Inbox { body }
        }
        AgentWrite::InboxStatus { id, status } => {
            if !crate::domain::is_uuid(&id) {
                problems.push("id must be a UUID.".into());
            }
            if !OWNER_STATUSES.contains(&status.as_str()) {
                problems.push(format!("status must be one of: {}.", OWNER_STATUSES.join(", ")));
            }
            Valid::InboxStatus { id, status }
        }
        AgentWrite::Message { message_kind, title, body, slot } => {
            if !MESSAGE_KINDS.contains(&message_kind.as_str()) {
                problems.push(format!("message_kind must be one of: {}.", MESSAGE_KINDS.join(", ")));
            }
            let slot = slot.unwrap_or_else(|| "manual".into());
            if !SLOTS.contains(&slot.as_str()) {
                problems.push(format!("slot must be one of: {}.", SLOTS.join(", ")));
            }
            let (title, body) = (title.trim().to_string(), body.trim().to_string());
            length_problem("title", &title, TITLE_MAX, &mut problems);
            length_problem("body", &body, MESSAGE_MAX, &mut problems);
            Valid::Message { kind: message_kind, slot, title, body }
        }
        AgentWrite::Switch { enabled } => Valid::Switch { enabled },
        AgentWrite::TokenCreate { label } => {
            let label = label.trim().to_string();
            length_problem("label", &label, LABEL_MAX, &mut problems);
            Valid::TokenCreate { label }
        }
        AgentWrite::TokenRevoke { id } => {
            if !crate::domain::is_uuid(&id) {
                problems.push("id must be a UUID.".into());
            }
            Valid::TokenRevoke { id }
        }
        AgentWrite::ProposalDismiss { id } => {
            if !crate::domain::is_uuid(&id) {
                problems.push("id must be a UUID.".into());
            }
            Valid::ProposalDismiss { id }
        }
    };
    if problems.is_empty() { Ok(valid) } else { Err(problems) }
}

fn json(text: String) -> Value {
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

const INBOX_JSON: &str = "jsonb_build_object('id', i.id::text, 'created_at', i.created_at, 'updated_at', i.updated_at, \
    'body', i.body, 'status', i.status, 'reply', i.reply, 'proposal', i.proposal, 'proposal_status', i.proposal_status, \
    'proposal_event_id', i.proposal_event_id::text)";

pub enum WriteError {
    NotFound,
    /// The item exists but is not in a state that allows this change (for example a run that already finished).
    Conflict,
    /// Too many live runner tokens.
    LimitReached,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for WriteError {
    fn from(e: sqlx::Error) -> Self {
        WriteError::Db(e)
    }
}

pub async fn create_inbox(pool: &PgPool, body: &str, ip: Option<&str>) -> Result<Value, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let text: String = sqlx::query_scalar(&format!(
        "WITH i AS (INSERT INTO lifeui.agent_inbox (body) VALUES ($1) RETURNING *) SELECT {INBOX_JSON}::text FROM i"
    ))
    .bind(body)
    .persistent(false)
    .fetch_one(&mut *tx)
    .await?;
    let created = json(text);
    store::audit_in(&mut tx, "agent_inbox_create", created["id"].as_str(), ip, serde_json::json!({})).await?;
    tx.commit().await?;
    Ok(created)
}

pub async fn set_inbox_status(pool: &PgPool, id: &str, status: &str, ip: Option<&str>) -> Result<Value, WriteError> {
    let mut tx = pool.begin().await?;
    let text: Option<String> = sqlx::query_scalar(&format!(
        "WITH i AS (UPDATE lifeui.agent_inbox SET status = $2, updated_at = now() WHERE id = $1::uuid RETURNING *) \
         SELECT {INBOX_JSON}::text FROM i"
    ))
    .bind(id)
    .bind(status)
    .persistent(false)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(text) = text else {
        tx.rollback().await?;
        return Err(WriteError::NotFound);
    };
    store::audit_in(&mut tx, "agent_inbox_update", Some(id), ip, serde_json::json!({ "status": status })).await?;
    tx.commit().await?;
    Ok(json(text))
}

/// The owner dismisses a pending proposal: the inbox item is closed as cancelled (Reopen sends it back to the agent,
/// which may then propose again).
pub async fn dismiss_proposal(pool: &PgPool, id: &str, ip: Option<&str>) -> Result<Value, WriteError> {
    let mut tx = pool.begin().await?;
    let text: Option<String> = sqlx::query_scalar(&format!(
        "WITH i AS (UPDATE lifeui.agent_inbox SET proposal_status = 'dismissed', status = 'cancelled', updated_at = now() \
                    WHERE id = $1::uuid AND proposal_status = 'pending' RETURNING *) SELECT {INBOX_JSON}::text FROM i"
    ))
    .bind(id)
    .persistent(false)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(text) = text else {
        tx.rollback().await?;
        return Err(WriteError::Conflict);
    };
    store::audit_in(&mut tx, "agent_inbox_update", Some(id), ip, serde_json::json!({ "status": "cancelled", "proposal": "dismissed" })).await?;
    tx.commit().await?;
    Ok(json(text))
}

pub async fn create_message(pool: &PgPool, kind: &str, slot: &str, title: &str, body: &str) -> Result<Value, sqlx::Error> {
    let text: String = sqlx::query_scalar(
        "WITH m AS (INSERT INTO lifeui.agent_messages (kind, slot, title, body) VALUES ($1, $2, $3, $4) RETURNING *) \
         SELECT jsonb_build_object('id', m.id::text, 'created_at', m.created_at, 'kind', m.kind, 'slot', m.slot, 'title', m.title, \
                'body', m.body, 'origin', m.origin)::text FROM m",
    )
    .bind(kind)
    .bind(slot)
    .bind(title)
    .bind(body)
    .persistent(false)
    .fetch_one(pool)
    .await?;
    Ok(json(text))
}

/// Inbox items: open ones first (new, picked up, needs answer), then finished, newest first inside each group.
pub async fn list_inbox(pool: &PgPool, limit: i64) -> Result<Value, sqlx::Error> {
    let text: String = sqlx::query_scalar(&format!(
        "SELECT coalesce(jsonb_agg(t.j ORDER BY t.closed, t.created_at DESC, t.id), '[]'::jsonb)::text FROM ( \
           SELECT {INBOX_JSON} AS j, (i.status IN ('done', 'cancelled')) AS closed, i.created_at, i.id \
           FROM lifeui.agent_inbox i ORDER BY (i.status IN ('done', 'cancelled')), i.created_at DESC, i.id LIMIT $1) t"
    ))
    .bind(limit)
    .persistent(false)
    .fetch_one(pool)
    .await?;
    Ok(json(text))
}

pub async fn list_feed(pool: &PgPool, limit: i64) -> Result<Value, sqlx::Error> {
    let text: String = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(t.j ORDER BY t.created_at DESC, t.id DESC), '[]'::jsonb)::text FROM ( \
           SELECT jsonb_build_object('id', m.id::text, 'created_at', m.created_at, 'kind', m.kind, 'slot', m.slot, 'title', m.title, \
                  'body', m.body, 'origin', m.origin) AS j, m.created_at, m.id \
           FROM lifeui.agent_messages m ORDER BY m.created_at DESC, m.id DESC LIMIT $1) t",
    )
    .bind(limit)
    .persistent(false)
    .fetch_one(pool)
    .await?;
    Ok(json(text))
}

pub async fn list_runs(pool: &PgPool, limit: i64) -> Result<Value, sqlx::Error> {
    let text: String = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(t.j ORDER BY t.started_at DESC, t.id DESC), '[]'::jsonb)::text FROM ( \
           SELECT jsonb_build_object('id', r.id::text, 'started_at', r.started_at, 'finished_at', r.finished_at, 'slot', r.slot, \
                  'status', r.status, 'summary', r.summary, 'tool_calls', r.tool_calls, 'tokens', r.tokens) AS j, r.started_at, r.id \
           FROM lifeui.agent_runs r ORDER BY r.started_at DESC, r.id DESC LIMIT $1) t",
    )
    .bind(limit)
    .persistent(false)
    .fetch_one(pool)
    .await?;
    Ok(json(text))
}

// ---------------------------------------------------------------- the kill switch and the runner tokens (owner side)

/// Is the agent switched on? Off until the owner turns it on.
pub async fn is_enabled(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let on: Option<bool> = sqlx::query_scalar("SELECT enabled FROM lifeui.agent_settings WHERE id = 1")
        .persistent(false)
        .fetch_optional(pool)
        .await?;
    Ok(on.unwrap_or(false))
}

pub async fn set_enabled(pool: &PgPool, enabled: bool, ip: Option<&str>) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO lifeui.agent_settings (id, enabled) VALUES (1, $1) \
         ON CONFLICT (id) DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now()",
    )
    .bind(enabled)
    .persistent(false)
    .execute(&mut *tx)
    .await?;
    store::audit_in(&mut tx, "agent_switch", None, ip, serde_json::json!({ "enabled": enabled })).await?;
    tx.commit().await?;
    Ok(())
}

/// Settings view: the switch and every token (never the token itself or its hash).
pub async fn settings(pool: &PgPool) -> Result<Value, sqlx::Error> {
    let enabled = is_enabled(pool).await?;
    let text: String = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(jsonb_build_object('id', t.id::text, 'label', t.label, 'created_at', t.created_at, \
                'last_used_at', t.last_used_at, 'revoked_at', t.revoked_at) ORDER BY t.created_at DESC), '[]'::jsonb)::text \
         FROM lifeui.agent_tokens t",
    )
    .persistent(false)
    .fetch_one(pool)
    .await?;
    Ok(serde_json::json!({ "enabled": enabled, "tokens": json(text) }))
}

/// Stores the SHA-256 of a new runner token. The caller shows the token to the owner once and forgets it.
pub async fn create_token(pool: &PgPool, label: &str, token_hash: &str, ip: Option<&str>) -> Result<String, WriteError> {
    let mut tx = pool.begin().await?;
    // serialise creators so two requests cannot both slip under the limit
    sqlx::query("LOCK TABLE lifeui.agent_tokens IN SHARE ROW EXCLUSIVE MODE").persistent(false).execute(&mut *tx).await?;
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM lifeui.agent_tokens WHERE revoked_at IS NULL")
        .persistent(false)
        .fetch_one(&mut *tx)
        .await?;
    if active >= MAX_ACTIVE_TOKENS {
        tx.rollback().await?;
        return Err(WriteError::LimitReached);
    }
    let id: String = sqlx::query_scalar("INSERT INTO lifeui.agent_tokens (label, token_hash) VALUES ($1, $2) RETURNING id::text")
        .bind(label)
        .bind(token_hash)
        .persistent(false)
        .fetch_one(&mut *tx)
        .await?;
    store::audit_in(&mut tx, "agent_token_create", Some(&id), ip, serde_json::json!({})).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn revoke_token(pool: &PgPool, id: &str, ip: Option<&str>) -> Result<(), WriteError> {
    let mut tx = pool.begin().await?;
    let done = sqlx::query("UPDATE lifeui.agent_tokens SET revoked_at = now() WHERE id = $1::uuid AND revoked_at IS NULL")
        .bind(id)
        .persistent(false)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if done != 1 {
        tx.rollback().await?;
        return Err(WriteError::NotFound);
    }
    store::audit_in(&mut tx, "agent_token_revoke", Some(id), ip, serde_json::json!({})).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------- the runner API (what the scheduled agent may do)

/// What POST /api/runner accepts, with a runner token. Deliberately small: the runner can open and close its own
/// run, take inbox items, answer them and post to the feed. It cannot read or write events or the
/// knowledge base (those scopes come later, one at a time).
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunnerWrite {
    RunStart {
        slot: String,
    },
    RunFinish {
        run_id: String,
        status: String,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        tool_calls: i64,
        #[serde(default)]
        tokens: i64,
    },
    /// Takes the open inbox items (status new -> picked_up) for this run.
    InboxClaim {
        run_id: String,
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Answers an item the run has picked up: done, or needs_answer when it must ask the owner something.
    /// With `proposal` the agent PREPARES an event (stored as text on the item); only the owner can add it.
    InboxReply {
        id: String,
        status: String,
        reply: String,
        #[serde(default)]
        proposal: Option<EventProposal>,
    },
    Message {
        run_id: String,
        message_kind: String,
        #[serde(default)]
        slot: Option<String>,
        title: String,
        body: String,
    },
    /// Read-only snapshot for the briefing: nearby events and open tasks. Never the Vault; nothing Sensitive or Secret.
    Context { run_id: String },
}

/// An event the agent proposes from an inbox item. Local wall-clock date and time in the owner's time zone (the app turns them into an
/// instant when the owner adds it). Deliberately smaller than a full event: no sensitivity, payload or supersedes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventProposal {
    pub class: String,
    pub name: String,
    pub date: String,
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Classes the agent may propose (money and measurements need numbers the owner should enter themselves).
pub const PROPOSAL_CLASSES: [&str; 5] = ["Event", "Appointment", "Meeting", "Trip", "Milestone"];

fn validate_proposal(p: EventProposal, problems: &mut Vec<String>) -> Value {
    if !PROPOSAL_CLASSES.contains(&p.class.as_str()) {
        problems.push(format!("proposal.class must be one of: {}.", PROPOSAL_CLASSES.join(", ")));
    }
    let name = p.name.trim().to_string();
    length_problem("proposal.name", &name, crate::domain::MAX_NAME_CHARS, problems);
    let date_ok = time::Date::parse(&p.date, &time::format_description::well_known::Iso8601::DATE).is_ok() && p.date.len() == 10;
    if !date_ok {
        problems.push("proposal.date must be a date such as 2026-05-29.".into());
    }
    let time = p.time.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    if let Some(t) = &time {
        let b = t.as_bytes();
        let ok = b.len() == 5
            && b[2] == b':'
            && t[..2].parse::<u8>().is_ok_and(|h| h < 24 && b[0].is_ascii_digit() && b[1].is_ascii_digit())
            && t[3..].parse::<u8>().is_ok_and(|m| m < 60 && b[3].is_ascii_digit() && b[4].is_ascii_digit());
        if !ok {
            problems.push("proposal.time must be HH:MM (24 h), such as 15:00.".into());
        }
    }
    let description = p.description.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
    if description.as_ref().is_some_and(|d| d.chars().count() > crate::domain::MAX_DESCRIPTION_CHARS) {
        problems.push(format!("proposal.description must have at most {} characters.", crate::domain::MAX_DESCRIPTION_CHARS));
    }
    let mut out = serde_json::json!({ "class": p.class, "name": name, "date": p.date });
    if let Some(t) = time {
        out["time"] = Value::String(t);
    }
    if let Some(d) = description {
        out["description"] = Value::String(d);
    }
    out
}

#[derive(Debug, PartialEq)]
pub enum RunnerValid {
    Context { run_id: String },
    RunStart { slot: String },
    RunFinish { run_id: String, status: String, summary: Option<String>, tool_calls: i32, tokens: i32 },
    InboxClaim { run_id: String, limit: i64 },
    InboxReply { id: String, status: String, reply: String, proposal: Option<Value> },
    Message { run_id: String, kind: String, slot: String, title: String, body: String },
}

/// Slots a scheduled run can have ('manual' is only for hand-written test messages).
pub const RUN_SLOTS: [&str; 3] = ["morning", "midday", "evening"];
pub const RUNNER_FINISH_STATUSES: [&str; 3] = ["ok", "failed", "skipped"];
pub const RUNNER_REPLY_STATUSES: [&str; 2] = ["done", "needs_answer"];
pub const SUMMARY_MAX: usize = 2000;
const COUNTER_MAX: i64 = 10_000_000;

pub fn validate_runner(input: RunnerWrite) -> Result<RunnerValid, Vec<String>> {
    let mut problems = Vec::new();
    let valid = match input {
        RunnerWrite::RunStart { slot } => {
            if !RUN_SLOTS.contains(&slot.as_str()) {
                problems.push(format!("slot must be one of: {}.", RUN_SLOTS.join(", ")));
            }
            RunnerValid::RunStart { slot }
        }
        RunnerWrite::RunFinish { run_id, status, summary, tool_calls, tokens } => {
            uuid_problem("run_id", &run_id, &mut problems);
            if !RUNNER_FINISH_STATUSES.contains(&status.as_str()) {
                problems.push(format!("status must be one of: {}.", RUNNER_FINISH_STATUSES.join(", ")));
            }
            let summary = summary.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            if summary.as_ref().is_some_and(|s| s.chars().count() > SUMMARY_MAX) {
                problems.push(format!("summary must have at most {SUMMARY_MAX} characters."));
            }
            for (name, value) in [("tool_calls", tool_calls), ("tokens", tokens)] {
                if !(0..=COUNTER_MAX).contains(&value) {
                    problems.push(format!("{name} must be a whole number from 0 to {COUNTER_MAX}."));
                }
            }
            RunnerValid::RunFinish {
                run_id,
                status,
                summary,
                tool_calls: tool_calls.clamp(0, COUNTER_MAX) as i32,
                tokens: tokens.clamp(0, COUNTER_MAX) as i32,
            }
        }
        RunnerWrite::InboxClaim { run_id, limit } => {
            uuid_problem("run_id", &run_id, &mut problems);
            let limit = limit.unwrap_or(20);
            if !(1..=50).contains(&limit) {
                problems.push("limit must be a whole number from 1 to 50.".into());
            }
            RunnerValid::InboxClaim { run_id, limit: limit.clamp(1, 50) }
        }
        RunnerWrite::InboxReply { id, status, reply, proposal } => {
            uuid_problem("id", &id, &mut problems);
            if !RUNNER_REPLY_STATUSES.contains(&status.as_str()) {
                problems.push(format!("status must be one of: {}.", RUNNER_REPLY_STATUSES.join(", ")));
            }
            let reply = reply.trim().to_string();
            length_problem("reply", &reply, INBOX_MAX, &mut problems);
            let proposal = proposal.map(|p| validate_proposal(p, &mut problems));
            if proposal.is_some() && status != "needs_answer" {
                problems.push("An item with a proposal waits for the owner: status must be needs_answer.".into());
            }
            RunnerValid::InboxReply { id, status, reply, proposal }
        }
        RunnerWrite::Context { run_id } => {
            uuid_problem("run_id", &run_id, &mut problems);
            RunnerValid::Context { run_id }
        }
        RunnerWrite::Message { run_id, message_kind, slot, title, body } => {
            uuid_problem("run_id", &run_id, &mut problems);
            if !MESSAGE_KINDS.contains(&message_kind.as_str()) {
                problems.push(format!("message_kind must be one of: {}.", MESSAGE_KINDS.join(", ")));
            }
            let slot = slot.unwrap_or_else(|| "manual".into());
            if !SLOTS.contains(&slot.as_str()) {
                problems.push(format!("slot must be one of: {}.", SLOTS.join(", ")));
            }
            let (title, body) = (title.trim().to_string(), body.trim().to_string());
            length_problem("title", &title, TITLE_MAX, &mut problems);
            length_problem("body", &body, MESSAGE_MAX, &mut problems);
            RunnerValid::Message { run_id, kind: message_kind, slot, title, body }
        }
    };
    if problems.is_empty() { Ok(valid) } else { Err(problems) }
}

fn uuid_problem(name: &str, value: &str, problems: &mut Vec<String>) {
    if !crate::domain::is_uuid(value) {
        problems.push(format!("{name} must be a UUID."));
    }
}

/// Checks a runner token and notes that it was used. Returns the token's id, or None for an unknown or revoked one.
pub async fn token_id_for(pool: &PgPool, token_hash: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("UPDATE lifeui.agent_tokens SET last_used_at = now() WHERE token_hash = $1 AND revoked_at IS NULL RETURNING id::text")
        .bind(token_hash)
        .persistent(false)
        .fetch_optional(pool)
        .await
}

const RUN_JSON: &str = "jsonb_build_object('id', r.id::text, 'started_at', r.started_at, 'finished_at', r.finished_at, 'slot', r.slot, \
    'status', r.status, 'summary', r.summary, 'tool_calls', r.tool_calls, 'tokens', r.tokens)";

pub async fn run_start(pool: &PgPool, slot: &str, token_id: &str) -> Result<Value, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let text: String = sqlx::query_scalar(&format!("WITH r AS (INSERT INTO lifeui.agent_runs (slot) VALUES ($1) RETURNING *) SELECT {RUN_JSON}::text FROM r"))
        .bind(slot)
        .persistent(false)
        .fetch_one(&mut *tx)
        .await?;
    let run = json(text);
    store::audit_in(&mut tx, "agent_run", run["id"].as_str(), None, serde_json::json!({ "phase": "start", "slot": slot, "token": token_id })).await?;
    tx.commit().await?;
    Ok(run)
}

pub async fn run_finish(
    pool: &PgPool,
    run_id: &str,
    status: &str,
    summary: Option<&str>,
    tool_calls: i32,
    tokens: i32,
) -> Result<Value, WriteError> {
    let mut tx = pool.begin().await?;
    let text: Option<String> = sqlx::query_scalar(&format!(
        "WITH r AS (UPDATE lifeui.agent_runs SET status = $2, summary = $3, tool_calls = $4, tokens = $5, finished_at = now() \
                    WHERE id = $1::uuid AND status = 'running' RETURNING *) SELECT {RUN_JSON}::text FROM r"
    ))
    .bind(run_id)
    .bind(status)
    .bind(summary)
    .bind(tool_calls)
    .bind(tokens)
    .persistent(false)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(text) = text else {
        tx.rollback().await?;
        return Err(WriteError::Conflict);
    };
    store::audit_in(&mut tx, "agent_run", Some(run_id), None, serde_json::json!({ "phase": "finish", "status": status })).await?;
    tx.commit().await?;
    Ok(json(text))
}

pub async fn inbox_claim(pool: &PgPool, run_id: &str, limit: i64) -> Result<Value, WriteError> {
    let mut tx = pool.begin().await?;
    let running: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lifeui.agent_runs WHERE id = $1::uuid AND status = 'running')")
        .bind(run_id)
        .persistent(false)
        .fetch_one(&mut *tx)
        .await?;
    if !running {
        tx.rollback().await?;
        return Err(WriteError::Conflict);
    }
    let text: String = sqlx::query_scalar(&format!(
        "WITH c AS (SELECT id FROM lifeui.agent_inbox WHERE status = 'new' ORDER BY created_at, id LIMIT $2 FOR UPDATE SKIP LOCKED), \
              u AS (UPDATE lifeui.agent_inbox i SET status = 'picked_up', run_id = $1::uuid, updated_at = now() FROM c WHERE i.id = c.id RETURNING i.*) \
         SELECT coalesce(jsonb_agg({INBOX_JSON} ORDER BY i.created_at, i.id), '[]'::jsonb)::text FROM u i"
    ))
    .bind(run_id)
    .bind(limit)
    .persistent(false)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json(text))
}

/// A proposal is stored only as text; one that was already ADDED (an item reopened later) is never replaced.
pub async fn inbox_reply(pool: &PgPool, id: &str, status: &str, reply: &str, proposal: Option<&Value>) -> Result<Value, WriteError> {
    let text: Option<String> = sqlx::query_scalar(&format!(
        "WITH i AS (UPDATE lifeui.agent_inbox SET status = $2, reply = $3, updated_at = now(), \
                      proposal = CASE WHEN $4::jsonb IS NOT NULL AND proposal_status IS DISTINCT FROM 'added' THEN $4::jsonb ELSE proposal END, \
                      proposal_status = CASE WHEN $4::jsonb IS NOT NULL AND proposal_status IS DISTINCT FROM 'added' THEN 'pending' ELSE proposal_status END \
                    WHERE id = $1::uuid AND status = 'picked_up' RETURNING *) \
         SELECT {INBOX_JSON}::text FROM i"
    ))
    .bind(id)
    .bind(status)
    .bind(reply)
    .bind(proposal.map(|p| p.to_string()))
    .persistent(false)
    .fetch_optional(pool)
    .await?;
    text.map(json).ok_or(WriteError::Conflict)
}

/// A feed message from the agent (origin 'agent'); only while the named run is still running.
pub async fn agent_message(pool: &PgPool, run_id: &str, kind: &str, slot: &str, title: &str, body: &str) -> Result<Value, WriteError> {
    let text: Option<String> = sqlx::query_scalar(
        "WITH m AS (INSERT INTO lifeui.agent_messages (kind, slot, title, body, origin, run_id) \
                    SELECT $2, $3, $4, $5, 'agent', r.id FROM lifeui.agent_runs r WHERE r.id = $1::uuid AND r.status = 'running' RETURNING *) \
         SELECT jsonb_build_object('id', m.id::text, 'created_at', m.created_at, 'kind', m.kind, 'slot', m.slot, 'title', m.title, \
                'body', m.body, 'origin', m.origin)::text FROM m",
    )
    .bind(run_id)
    .bind(kind)
    .bind(slot)
    .bind(title)
    .bind(body)
    .persistent(false)
    .fetch_optional(pool)
    .await?;
    text.map(json).ok_or(WriteError::Conflict)
}

/// Rows the briefing may see (the runner never gets more than this).
const CONTEXT_EVENT_LIMIT: i64 = 60;
const CONTEXT_TASK_LIMIT: i64 = 60;
const CONTEXT_TEXT_MAX: i32 = 300;

/// Read-only snapshot for a running run: events from 3 days back to 14 days ahead and the open tasks.
/// Only 'Public' and 'Personal' rows are included: 'Sensitive' and 'Secret' never leave the database through the
/// runner.
pub async fn runner_context(pool: &PgPool, run_id: &str) -> Result<Value, WriteError> {
    let running: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lifeui.agent_runs WHERE id = $1::uuid AND status = 'running')")
        .bind(run_id)
        .persistent(false)
        .fetch_one(pool)
        .await?;
    if !running {
        return Err(WriteError::Conflict);
    }
    let text: String = sqlx::query_scalar(
        "SELECT jsonb_build_object(            'now', now(),            'events', coalesce((SELECT jsonb_agg(t.j ORDER BY t.k, t.id) FROM (                SELECT jsonb_build_object('class', regexp_replace(e.class_iri, '^.*#', ''), 'name', e.name,                       'description', left(e.description, $2), 'status', e.status,                       'when', COALESCE(e.occurred_at, e.started_at, e.occurred_on::timestamptz), 'ends', e.ended_at) AS j,                       COALESCE(e.occurred_at, e.started_at, e.occurred_on::timestamptz) AS k, e.id                FROM lifeui.events e                WHERE e.superseded_by IS NULL AND e.sensitivity IN ('Public', 'Personal')                  AND COALESCE(e.occurred_at, e.started_at, e.occurred_on::timestamptz) >= now() - interval '3 days'                  AND COALESCE(e.occurred_at, e.started_at, e.occurred_on::timestamptz) < now() + interval '14 days'                ORDER BY k LIMIT $3) t), '[]'::jsonb),            'tasks', coalesce((SELECT jsonb_agg(t.j ORDER BY t.due NULLS LAST, t.name) FROM (                SELECT jsonb_build_object('name', e.name, 'status', e.status, 'due', e.props->'dueDate', 'priority', e.props->'priority',                       'description', left(e.props->>'description', $2)) AS j, e.props->>'dueDate' AS due, e.name                FROM lifeui.entities e                WHERE e.class_iri LIKE '%#Task' AND e.sensitivity IN ('Public', 'Personal')                  AND coalesce(lower(e.status), '') NOT IN ('done', 'completed', 'cancelled', 'archived')                ORDER BY e.props->>'dueDate' NULLS LAST, e.name LIMIT $4) t), '[]'::jsonb)          )::text AS j WHERE $1::uuid IS NOT NULL",
    )
    .bind(run_id)
    .bind(CONTEXT_TEXT_MAX)
    .bind(CONTEXT_EVENT_LIMIT)
    .bind(CONTEXT_TASK_LIMIT)
    .persistent(false)
    .fetch_one(pool)
    .await?;
    Ok(json(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> AgentWrite {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn inbox_text_is_trimmed_and_bounded() {
        assert_eq!(
            validate(parse(r#"{"kind":"inbox","body":"  add dentist Friday 15:00 "}"#)),
            Ok(Valid::Inbox { body: "add dentist Friday 15:00".into() })
        );
        assert!(validate(parse(r#"{"kind":"inbox","body":"   "}"#)).is_err(), "blank is refused");
        let long = format!(r#"{{"kind":"inbox","body":"{}"}}"#, "x".repeat(INBOX_MAX + 1));
        assert!(validate(parse(&long)).is_err());
        let ok = format!(r#"{{"kind":"inbox","body":"{}"}}"#, "é".repeat(INBOX_MAX));
        assert!(validate(parse(&ok)).is_ok(), "the limit counts characters, not bytes");
    }

    #[test]
    fn the_owner_can_only_set_owner_statuses() {
        let id = "123e4567-e89b-12d3-a456-426614174000";
        for status in OWNER_STATUSES {
            assert!(validate(parse(&format!(r#"{{"kind":"inbox_status","id":"{id}","status":"{status}"}}"#))).is_ok());
        }
        assert!(validate(parse(&format!(r#"{{"kind":"inbox_status","id":"{id}","status":"picked_up"}}"#))).is_err());
        assert!(validate(parse(r#"{"kind":"inbox_status","id":"nope","status":"done"}"#)).is_err());
    }

    #[test]
    fn messages_need_a_known_kind_and_slot() {
        assert!(validate(parse(r#"{"kind":"message","message_kind":"briefing","title":"Morning","body":"Today: ..."}"#)).is_ok());
        assert!(validate(parse(r#"{"kind":"message","message_kind":"gossip","title":"t","body":"b"}"#)).is_err());
        assert!(validate(parse(r#"{"kind":"message","message_kind":"note","slot":"night","title":"t","body":"b"}"#)).is_err());
        assert!(serde_json::from_str::<AgentWrite>(r#"{"kind":"inbox","body":"x","status":"done"}"#).is_err(), "unknown fields are refused");
    }

    fn runner(json: &str) -> RunnerWrite {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_runner_can_only_start_scheduled_slots() {
        for slot in RUN_SLOTS {
            assert!(validate_runner(runner(&format!(r#"{{"action":"run_start","slot":"{slot}"}}"#))).is_ok());
        }
        assert!(validate_runner(runner(r#"{"action":"run_start","slot":"manual"}"#)).is_err());
        assert!(validate_runner(runner(r#"{"action":"run_start","slot":"night"}"#)).is_err());
    }

    #[test]
    fn the_runner_cannot_do_anything_outside_its_small_set() {
        for bad in [r#"{"action":"create_event"}"#, r#"{"action":"vault_read"}"#, r#"{"action":"context"}"#, r#"{"action":"run_start","slot":"morning","extra":1}"#] {
            assert!(serde_json::from_str::<RunnerWrite>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn runner_answers_are_bounded() {
        let id = "123e4567-e89b-12d3-a456-426614174000";
        assert!(validate_runner(runner(&format!(r#"{{"action":"inbox_reply","id":"{id}","status":"done","reply":"Added."}}"#))).is_ok());
        assert!(validate_runner(runner(&format!(r#"{{"action":"inbox_reply","id":"{id}","status":"new","reply":"x"}}"#))).is_err(), "only done or needs_answer");
        assert!(validate_runner(runner(&format!(r#"{{"action":"inbox_reply","id":"{id}","status":"done","reply":" "}}"#))).is_err());
        assert!(validate_runner(runner(&format!(r#"{{"action":"inbox_claim","run_id":"{id}","limit":51}}"#))).is_err());
        assert!(validate_runner(runner(&format!(r#"{{"action":"run_finish","run_id":"{id}","status":"ok","tokens":-1}}"#))).is_err());
        assert!(validate_runner(runner(&format!(r#"{{"action":"run_finish","run_id":"{id}","status":"running"}}"#))).is_err());
        assert_eq!(
            validate_runner(runner(&format!(r#"{{"action":"run_finish","run_id":"{id}","status":"ok","summary":"  fine ","tool_calls":3,"tokens":900}}"#))),
            Ok(RunnerValid::RunFinish { run_id: id.into(), status: "ok".into(), summary: Some("fine".into()), tool_calls: 3, tokens: 900 })
        );
    }

    #[test]
    fn proposals_are_small_checked_and_wait_for_the_owner() {
        let id = "123e4567-e89b-12d3-a456-426614174000";
        let reply = |status: &str, proposal: &str| {
            validate_runner(runner(&format!(r#"{{"action":"inbox_reply","id":"{id}","status":"{status}","reply":"Prepared.","proposal":{proposal}}}"#)))
        };
        let ok = reply("needs_answer", r#"{"class":"Appointment","name":" Dentist ","date":"2026-11-27","time":"15:00"}"#).unwrap();
        let RunnerValid::InboxReply { proposal: Some(p), .. } = ok else { panic!("proposal kept") };
        assert_eq!(p, serde_json::json!({"class": "Appointment", "name": "Dentist", "date": "2026-11-27", "time": "15:00"}));
        assert!(reply("needs_answer", r#"{"class":"Event","name":"All day","date":"2026-11-27"}"#).is_ok(), "time is optional");
        assert!(reply("done", r#"{"class":"Event","name":"x","date":"2026-11-27"}"#).is_err(), "the owner still has to add it");
        for bad in [
            r#"{"class":"Transaction","name":"x","date":"2026-11-27"}"#,
            r#"{"class":"Event","name":" ","date":"2026-11-27"}"#,
            r#"{"class":"Event","name":"x","date":"2026-02-30"}"#,
            r#"{"class":"Event","name":"x","date":"27/11/2026"}"#,
            r#"{"class":"Event","name":"x","date":"2026-11-27","time":"25:00"}"#,
            r#"{"class":"Event","name":"x","date":"2026-11-27","time":"3pm"}"#,
        ] {
            assert!(reply("needs_answer", bad).is_err(), "{bad}");
        }
        let extra = format!(r#"{{"action":"inbox_reply","id":"{id}","status":"needs_answer","reply":"x","proposal":{{"class":"Event","name":"x","date":"2026-11-27","sensitivity":"Public"}}}}"#);
        assert!(serde_json::from_str::<RunnerWrite>(&extra).is_err(), "unknown proposal fields are refused");
    }

    #[test]
    fn the_owner_dismisses_proposals_by_id() {
        assert!(validate(parse(r#"{"kind":"proposal_dismiss","id":"123e4567-e89b-12d3-a456-426614174000"}"#)).is_ok());
        assert!(validate(parse(r#"{"kind":"proposal_dismiss","id":"x"}"#)).is_err());
    }

    #[test]
    fn context_needs_a_run_and_takes_no_filters() {
        let id = "123e4567-e89b-12d3-a456-426614174000";
        assert!(validate_runner(runner(&format!(r#"{{"action":"context","run_id":"{id}"}}"#))).is_ok());
        assert!(validate_runner(runner(r#"{"action":"context","run_id":"nope"}"#)).is_err());
        assert!(serde_json::from_str::<RunnerWrite>(&format!(r#"{{"action":"context","run_id":"{id}","sql":"x"}}"#)).is_err());
    }

    #[test]
    fn token_labels_are_bounded() {
        assert!(validate(parse(r#"{"kind":"token_create","label":"my pc"}"#)).is_ok());
        assert!(validate(parse(r#"{"kind":"token_create","label":" "}"#)).is_err());
        assert!(validate(parse(&format!(r#"{{"kind":"token_create","label":"{}"}}"#, "x".repeat(LABEL_MAX + 1)))).is_err());
    }

    #[test]
    fn vocabularies_match_the_drizzle_schema() {
        let path: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "db/src/schema.ts"].iter().collect();
        let schema = std::fs::read_to_string(path).unwrap();
        let listed = |name: &str| -> Vec<String> {
            let start = schema.find(&format!("export const {name} = [")).unwrap_or_else(|| panic!("{name} not found in schema.ts"));
            let body = &schema[start..];
            body[body.find('[').unwrap() + 1..body.find(']').unwrap()]
                .split(',')
                .map(|s| s.trim().trim_matches('"').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let own = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(listed("AGENT_SLOTS"), own(&SLOTS));
        assert_eq!(listed("AGENT_INBOX_STATUS"), own(&INBOX_STATUSES));
        assert_eq!(listed("AGENT_MESSAGE_KINDS"), own(&MESSAGE_KINDS));
        assert_eq!(listed("AGENT_RUN_STATUS"), own(&RUN_STATUSES));
        assert_eq!(listed("AGENT_PROPOSAL_STATUS"), own(&PROPOSAL_STATUSES));
    }
}
