use lifeui_api::{
    agent::{self, RunnerValid, RunnerWrite, WriteError},
    auth,
    news::{self, BriefingInput},
    util::{
        Reply, bearer_token, error_reply, json_reply, method_not_allowed, pool_or_reply, query_params, read_json_max, server_error,
        validation_reply,
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

/// The scheduled agent's API. NOT for the browser: it takes `Authorization: Bearer <runner token>` (created in the
/// Agent tab) and ignores the login cookie. A runner token works here only; it cannot read or write events or
/// the knowledge base. Everything is refused while the kill switch in the Agent tab is off.
///   GET  /api/runner                                                        {"enabled": true|false, "stay_awake": {"stay_awake_on", "tonight"}}
///   GET  /api/runner?route=signals&days=7                                   daily news items of the last N days (1-120)
///        with the owner's read/saved/dismissed marks (weekly digest, monthly feedback job)
///   POST /api/runner/news  <briefing JSON from tools/news/news.py>          publish (replace) the briefing for its date
///        (vercel.json rewrites /api/runner/news to /api/runner?route=news: same function, same token and kill switch)
///   POST /api/runner {"action":"run_start","slot":"morning|midday|evening"}  open a run, answers with its id
///   POST /api/runner {"action":"inbox_claim","run_id":"..","limit":20}       take the new inbox items (new -> picked_up)
///   POST /api/runner {"action":"inbox_reply","id":"..","status":"done|needs_answer","reply":".."}
///   POST /api/runner {"action":"message","run_id":"..","message_kind":"briefing","slot":"morning","title":"..","body":".."}
///   POST /api/runner {"action":"context","run_id":".."}                      read-only snapshot: events -3..+14 days, open tasks
///   POST /api/runner {"action":"run_finish","run_id":"..","status":"ok|failed|skipped","summary":"..","tool_calls":3,"tokens":900}
async fn handler(req: Request) -> Reply {
    let pool = match pool_or_reply().await {
        Ok(pool) => pool,
        Err(reply) => return reply,
    };
    let Some(token) = bearer_token(&req) else {
        return error_reply(401, "Runner token required.");
    };
    let token_id = match agent::token_id_for(pool, &auth::hash_token(&token)).await {
        Ok(Some(id)) => id,
        Ok(None) => return error_reply(401, "Runner token required."),
        Err(e) => return server_error("runner token", e),
    };
    let enabled = match agent::is_enabled(pool).await {
        Ok(on) => on,
        Err(e) => return server_error("runner switch", e),
    };
    let is_news = req.uri().path().trim_end_matches('/').ends_with("/news")
        || query_params(&req).get("route").map(String::as_str) == Some("news");
    match req.method().as_str() {
        // The home server asks before its nightly shutdown; this works even while the agent is switched off.
        "GET" if query_params(&req).get("route").map(String::as_str) == Some("signals") => {
            let days = query_params(&req).get("days").and_then(|d| d.parse::<i32>().ok()).unwrap_or(7);
            if !(1..=120).contains(&days) {
                return validation_reply(vec!["days must be a whole number from 1 to 120.".into()]);
            }
            match news::signals(pool, days).await {
                Ok(items) => json_reply(200, json!({ "ok": true, "days": days, "items": items })),
                Err(e) => server_error("runner news signals", e),
            }
        }
        "GET" => match news::stay_awake(pool).await {
            Ok(stay) => json_reply(200, json!({ "ok": true, "enabled": enabled, "stay_awake": stay })),
            Err(e) => server_error("runner stay awake", e),
        },
        "POST" if !enabled => error_reply(403, "The agent is switched off (Agent tab)."),
        "POST" if is_news => publish_news(req, pool, &token_id).await,
        "POST" => act(req, pool, &token_id).await,
        _ => method_not_allowed("GET, POST"),
    }
}

async fn publish_news(req: Request, pool: &PgPool, token_id: &str) -> Reply {
    let input: BriefingInput = match read_json_max(req, news::PUBLISH_MAX_BYTES).await {
        Ok(input) => input,
        Err(message) => return error_reply(400, message),
    };
    let valid = match news::validate_briefing(input) {
        Ok(valid) => valid,
        Err(problems) => return validation_reply(problems),
    };
    match news::publish(pool, &valid, token_id).await {
        Ok(briefing) => json_reply(201, json!({ "ok": true, "briefing": briefing })),
        Err(e) => server_error("runner news publish", e),
    }
}

fn conflict(what: &str) -> Reply {
    error_reply(409, what)
}

async fn act(req: Request, pool: &PgPool, token_id: &str) -> Reply {
    let input: RunnerWrite = match read_json_max(req, MAX_BODY).await {
        Ok(input) => input,
        Err(message) => return error_reply(400, message),
    };
    let valid = match agent::validate_runner(input) {
        Ok(valid) => valid,
        Err(problems) => return validation_reply(problems),
    };
    match valid {
        RunnerValid::RunStart { slot } => match agent::run_start(pool, &slot, token_id).await {
            Ok(run) => json_reply(201, json!({ "ok": true, "run": run })),
            Err(e) => server_error("runner run_start", e),
        },
        RunnerValid::RunFinish { run_id, status, summary, tool_calls, tokens } => {
            match agent::run_finish(pool, &run_id, &status, summary.as_deref(), tool_calls, tokens).await {
                Ok(run) => json_reply(200, json!({ "ok": true, "run": run })),
                Err(WriteError::Db(e)) => server_error("runner run_finish", e),
                Err(_) => conflict("That run does not exist or has already finished."),
            }
        }
        RunnerValid::InboxClaim { run_id, limit } => match agent::inbox_claim(pool, &run_id, limit).await {
            Ok(items) => {
                let count = items.as_array().map_or(0, Vec::len);
                json_reply(200, json!({ "ok": true, "count": count, "items": items }))
            }
            Err(WriteError::Db(e)) => server_error("runner inbox_claim", e),
            Err(_) => conflict("That run does not exist or is not running."),
        },
        RunnerValid::InboxReply { id, status, reply, proposal } => match agent::inbox_reply(pool, &id, &status, &reply, proposal.as_ref()).await {
            Ok(item) => json_reply(200, json!({ "ok": true, "item": item })),
            Err(WriteError::Db(e)) => server_error("runner inbox_reply", e),
            Err(_) => conflict("Only an item this agent has picked up can be answered."),
        },
        RunnerValid::Context { run_id } => match agent::runner_context(pool, &run_id).await {
            Ok(context) => json_reply(200, json!({ "ok": true, "context": context })),
            Err(WriteError::Db(e)) => server_error("runner context", e),
            Err(_) => conflict("That run does not exist or is not running."),
        },
        RunnerValid::Message { run_id, kind, slot, title, body } => match agent::agent_message(pool, &run_id, &kind, &slot, &title, &body).await {
            Ok(message) => json_reply(201, json!({ "ok": true, "message": message })),
            Err(WriteError::Db(e)) => server_error("runner message", e),
            Err(_) => conflict("That run does not exist or is not running."),
        },
    }
}
