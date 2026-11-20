use lifeui_api::{
    auth, store,
    util::{Reply, client_ip, error_reply, guard_post, json_reply, pool_or_reply, rate_limited, read_json, server_error, start_session},
};
use serde::Deserialize;
use serde_json::json;
use vercel_runtime::{Error, Request, run, service_fn};

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}

#[derive(Deserialize)]
struct Body {
    setup_code: String,
    password: String,
}

/// First-run only: the owner chooses the password. Needs the setup code (env var SETUP_CODE) so a
/// stranger who finds the public URL first cannot claim the app. Refuses once a password exists.
async fn handler(req: Request) -> Reply {
    if let Some(reply) = guard_post(&req) {
        return reply;
    }
    let expected = match std::env::var("SETUP_CODE") {
        Ok(code) if code.trim().len() >= 16 => code,
        _ => return error_reply(503, "Setup is disabled: set SETUP_CODE (16+ characters) in the server environment first."),
    };
    let pool = match pool_or_reply().await {
        Ok(pool) => pool,
        Err(reply) => return reply,
    };
    let ip = client_ip(&req);
    let body: Body = match read_json(req).await {
        Ok(body) => body,
        Err(message) => return error_reply(400, message),
    };

    match store::owner_exists(pool).await {
        Ok(true) => return error_reply(409, "Already set up. Please log in."),
        Ok(false) => {}
        Err(e) => return server_error("auth_setup", e),
    }
    match store::lock_wait_secs(pool).await {
        Ok(wait) if wait > 0 => return rate_limited(wait),
        Ok(_) => {}
        Err(e) => return server_error("auth_setup", e),
    }
    if !auth::constant_time_eq(body.setup_code.trim(), expected.trim()) {
        let wait = store::record_failure(pool).await.unwrap_or(0);
        store::audit(pool, "setup_failed", None, ip.as_deref(), json!({ "retry_after_secs": wait })).await;
        return json_reply(403, json!({ "ok": false, "error": "Wrong setup code.", "retry_after_secs": wait }));
    }
    if let Err(message) = auth::validate_new_password(&body.password) {
        return error_reply(400, message);
    }

    let password = body.password;
    let hash = match tokio::task::spawn_blocking(move || auth::hash_password(&password)).await {
        Ok(Ok(hash)) => hash,
        Ok(Err(e)) => return server_error("auth_setup hash", e),
        Err(e) => return server_error("auth_setup hash task", e),
    };
    match store::create_owner(pool, &hash).await {
        Ok(true) => {}
        Ok(false) => return error_reply(409, "Already set up. Please log in."),
        Err(e) => return server_error("auth_setup", e),
    }
    let _ = store::reset_throttle(pool).await;
    store::audit(pool, "setup_ok", None, ip.as_deref(), json!({})).await;
    start_session(pool, json!({ "ok": true })).await
}
