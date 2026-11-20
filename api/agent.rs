use lifeui_api::{
    agent::{self, AgentWrite, Valid, WriteError},
    auth,
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

const MAX_BODY: usize = 32 * 1024;

/// The Agent tab. Login required for everything.
///   GET  /api/agent?view=inbox|feed|runs   (limit 1-200, default 50). inbox: open items first; feed and runs: newest first
///   POST /api/agent  {"kind":"inbox","body":"..."}                         give the agent something to do
///   POST /api/agent  {"kind":"inbox_status","id":"<uuid>","status":"done|cancelled|new"}
///   GET  /api/agent?view=settings          the kill switch and the runner tokens (never the tokens themselves)
///   POST /api/agent  {"kind":"switch","enabled":true|false}                kill switch (the agent starts OFF)
///   POST /api/agent  {"kind":"token_create","label":".."}                  new runner token, shown once in the answer
///   POST /api/agent  {"kind":"token_revoke","id":"<uuid>"}
///   POST /api/agent  {"kind":"proposal_dismiss","id":"<inbox id>"}          no to the event the agent proposed (item -> cancelled)
///                    (yes = POST /api/events with "from_proposal": "<inbox id>", the normal event checks apply)
///   POST /api/agent  {"kind":"message","message_kind":"note","title":"..","body":".."}   hand-written feed message (testing)
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
        "POST" => write(req, pool).await,
        _ => method_not_allowed("GET, POST"),
    }
}

async fn read(req: &Request, pool: &PgPool) -> Reply {
    let params = query_params(req);
    let mut problems = Vec::new();
    let view = params.get("view").map(String::as_str).unwrap_or("inbox");
    if !["inbox", "feed", "runs", "settings"].contains(&view) {
        problems.push("view must be one of: inbox, feed, runs, settings.".into());
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
    if view == "settings" {
        return match agent::settings(pool).await {
            Ok(settings) => json_reply(200, json!({ "ok": true, "view": view, "enabled": settings["enabled"], "tokens": settings["tokens"] })),
            Err(e) => server_error("agent settings", e),
        };
    }
    let rows = match view {
        "feed" => agent::list_feed(pool, limit).await,
        "runs" => agent::list_runs(pool, limit).await,
        _ => agent::list_inbox(pool, limit).await,
    };
    match rows {
        Ok(items) => {
            let count = items.as_array().map_or(0, Vec::len);
            json_reply(200, json!({ "ok": true, "view": view, "count": count, "items": items }))
        }
        Err(e) => server_error("agent list", e),
    }
}

async fn write(req: Request, pool: &PgPool) -> Reply {
    if let Some(reply) = check_origin(&req) {
        return reply;
    }
    let ip = client_ip(&req);
    let input: AgentWrite = match read_json_max(req, MAX_BODY).await {
        Ok(input) => input,
        Err(message) => return error_reply(400, message),
    };
    match agent::validate(input) {
        Err(problems) => validation_reply(problems),
        Ok(Valid::Inbox { body }) => match agent::create_inbox(pool, &body, ip.as_deref()).await {
            Ok(item) => json_reply(201, json!({ "ok": true, "item": item })),
            Err(e) => server_error("agent inbox create", e),
        },
        Ok(Valid::InboxStatus { id, status }) => match agent::set_inbox_status(pool, &id, &status, ip.as_deref()).await {
            Ok(item) => json_reply(200, json!({ "ok": true, "item": item })),
            Err(WriteError::NotFound) => error_reply(404, "No such inbox item."),
            Err(WriteError::Db(e)) => server_error("agent inbox status", e),
            Err(_) => error_reply(409, "Could not change the item."),
        },
        Ok(Valid::Switch { enabled }) => match agent::set_enabled(pool, enabled, ip.as_deref()).await {
            Ok(()) => json_reply(200, json!({ "ok": true, "enabled": enabled })),
            Err(e) => server_error("agent switch", e),
        },
        Ok(Valid::TokenCreate { label }) => {
            // the token is shown once, in this answer; only its SHA-256 is stored
            let token = match auth::new_session_token() {
                Ok(t) => format!("lua_{t}"),
                Err(e) => return server_error("agent token", e),
            };
            match agent::create_token(pool, &label, &auth::hash_token(&token), ip.as_deref()).await {
                Ok(id) => json_reply(201, json!({ "ok": true, "id": id, "token": token, "note": "Copy the token now: it is never shown again." })),
                Err(WriteError::LimitReached) => error_reply(409, &format!("At most {} runner tokens can be active; revoke one first.", agent::MAX_ACTIVE_TOKENS)),
                Err(WriteError::Db(e)) => server_error("agent token create", e),
                Err(_) => error_reply(409, "Could not create the token."),
            }
        }
        Ok(Valid::TokenRevoke { id }) => match agent::revoke_token(pool, &id, ip.as_deref()).await {
            Ok(()) => json_reply(200, json!({ "ok": true })),
            Err(WriteError::NotFound) => error_reply(404, "No such active token."),
            Err(WriteError::Db(e)) => server_error("agent token revoke", e),
            Err(_) => error_reply(409, "Could not revoke the token."),
        },
        Ok(Valid::ProposalDismiss { id }) => match agent::dismiss_proposal(pool, &id, ip.as_deref()).await {
            Ok(item) => json_reply(200, json!({ "ok": true, "item": item })),
            Err(WriteError::Db(e)) => server_error("agent proposal dismiss", e),
            Err(_) => error_reply(409, "That proposal was already added or dismissed."),
        },
        Ok(Valid::Message { kind, slot, title, body }) => match agent::create_message(pool, &kind, &slot, &title, &body).await {
            Ok(message) => json_reply(201, json!({ "ok": true, "message": message })),
            Err(e) => server_error("agent message create", e),
        },
    }
}
