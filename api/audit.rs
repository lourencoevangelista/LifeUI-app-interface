use lifeui_api::{
    store,
    util::{Reply, json_reply, method_not_allowed, pool_or_reply, query_params, require_auth, server_error, validation_reply},
};
use serde_json::json;
use vercel_runtime::{Error, Request, run, service_fn};

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}

/// The audit trail (who did what, when, from which IP). Read-only; login required; the rows are written by the
/// other functions and can never be changed or deleted through the app.
///   GET /api/audit      newest first. Filters: action (one of store::AUDIT_ACTIONS), limit (1-200, default 50)
async fn handler(req: Request) -> Reply {
    let pool = match pool_or_reply().await {
        Ok(pool) => pool,
        Err(reply) => return reply,
    };
    if let Err(reply) = require_auth(&req, pool).await {
        return reply;
    }
    if req.method().as_str() != "GET" {
        return method_not_allowed("GET");
    }
    let params = query_params(&req);

    let mut problems = Vec::new();
    let action = params.get("action").cloned().filter(|a| !a.is_empty());
    if action.as_deref().is_some_and(|a| !store::AUDIT_ACTIONS.contains(&a)) {
        problems.push(format!("action must be one of: {}.", store::AUDIT_ACTIONS.join(", ")));
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
    match store::list_audit(pool, action.as_deref(), limit).await {
        Ok(rows) => {
            let count = rows.as_array().map_or(0, Vec::len);
            json_reply(200, json!({ "ok": true, "count": count, "entries": rows }))
        }
        Err(e) => server_error("audit list", e),
    }
}
