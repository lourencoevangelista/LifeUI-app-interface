use lifeui_api::{
    data::{self, EventFilter, WriteError},
    domain::{self, EVENT_CLASSES, EventInput, STATUSES},
    shapes,
    util::{
        Reply, check_origin, client_ip, error_reply, json_reply, method_not_allowed, pool_or_reply, query_params, read_json_max,
        require_auth, server_error, validation_reply,
    },
};
use serde_json::json;
use sqlx::PgPool;
use vercel_runtime::{Error, Request, run, service_fn};

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}

const MAX_BODY: usize = 64 * 1024;

/// Events (the timeline). Login required for everything.
///   GET  /api/events                 timeline, newest first. Filters: class, status, from, to (exclusive), limit (1-200, default 50),
///                                    include_superseded=1
///   GET  /api/events?id=<uuid>       one event
///   POST /api/events                 create; with "supersedes": "<uuid>" it becomes a NEW VERSION of that event;
///                                    with "from_proposal": "<inbox id>" it is the Agent's proposed event, added by the owner
/// Nothing is updated in place or deleted: the replaced version keeps its row and is linked through superseded_by.
async fn handler(req: Request) -> Reply {
    let pool = match pool_or_reply().await {
        Ok(pool) => pool,
        Err(reply) => return reply,
    };
    if let Err(reply) = require_auth(&req, pool).await {
        return reply;
    }
    match req.method().as_str() {
        "GET" => read(&req, pool).await,
        "POST" => create(req, pool).await,
        _ => method_not_allowed("GET, POST"),
    }
}

async fn read(req: &Request, pool: &PgPool) -> Reply {
    let params = query_params(req);

    if let Some(id) = params.get("id") {
        if !domain::is_uuid(id) {
            return error_reply(400, "id must be a UUID.");
        }
        return match data::get_event(pool, id).await {
            Ok(Some(event)) => json_reply(200, json!({ "ok": true, "event": event })),
            Ok(None) => error_reply(404, "No such event."),
            Err(e) => server_error("events get", e),
        };
    }

    let mut problems = Vec::new();
    let class = params.get("class").cloned();
    if class.as_deref().is_some_and(|c| !EVENT_CLASSES.contains(&c)) {
        problems.push(format!("class must be one of: {}.", EVENT_CLASSES.join(", ")));
    }
    let status = params.get("status").cloned();
    if status.as_deref().is_some_and(|s| !STATUSES.contains(&s)) {
        problems.push(format!("status must be one of: {}.", STATUSES.join(", ")));
    }
    let from = params.get("from").cloned();
    let to = params.get("to").cloned();
    for (name, value) in [("from", &from), ("to", &to)] {
        if value.as_deref().is_some_and(|v| !domain::is_date_or_instant(v)) {
            problems.push(format!("{name} must be a date (2026-05-29) or an RFC 3339 timestamp."));
        }
    }
    let limit = match params.get("limit") {
        None => 50,
        Some(text) => match text.parse::<i64>() {
            Ok(n) if (1..=200).contains(&n) => n,
            _ => {
                problems.push("limit must be a whole number from 1 to 200.".into());
                50
            }
        },
    };
    if !problems.is_empty() {
        return validation_reply(problems);
    }
    let include_superseded = params.get("include_superseded").is_some_and(|v| v == "1" || v == "true");

    let filter = EventFilter { class, status, from, to, limit, include_superseded };
    match data::list_events(pool, &filter).await {
        Ok(events) => {
            let count = events.as_array().map_or(0, Vec::len);
            json_reply(200, json!({ "ok": true, "count": count, "events": events }))
        }
        Err(e) => server_error("events list", e),
    }
}

async fn create(req: Request, pool: &PgPool) -> Reply {
    if let Some(reply) = check_origin(&req) {
        return reply;
    }
    let ip = client_ip(&req);
    let input: EventInput = match read_json_max(req, MAX_BODY).await {
        Ok(input) => input,
        Err(message) => return error_reply(400, message),
    };
    let event = match domain::validate_event(input) {
        Ok(event) => event,
        Err(problems) => return validation_reply(problems),
    };
    // Authoritative gate: the real SHACL shapes (ontology/lifeui.shapes.ttl). CPU work, so off the async threads.
    let to_check = event.clone();
    match tokio::task::spawn_blocking(move || shapes::check_event(&to_check)).await {
        Ok(Ok(problems)) if problems.is_empty() => {}
        Ok(Ok(problems)) => return validation_reply(problems),
        Ok(Err(e)) => return server_error("events shapes", e),
        Err(e) => return server_error("events shapes task", e),
    }
    match data::create_event(pool, &event, ip.as_deref()).await {
        Ok(created) => json_reply(201, json!({ "ok": true, "event": created })),
        Err(WriteError::SupersedeConflict) => {
            error_reply(409, "supersedes must name an existing event that has not been replaced yet.")
        }
        Err(WriteError::ProposalConflict) => error_reply(409, "That proposal was already added or dismissed."),
        Err(WriteError::Rejected(rule)) => error_reply(400, &format!("Rejected by the database rule '{rule}'.")),
        Err(WriteError::Db(e)) => server_error("events create", e),
    }
}
