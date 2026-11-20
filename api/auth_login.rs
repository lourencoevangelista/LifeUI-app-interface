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
    password: String,
}

async fn handler(req: Request) -> Reply {
    if let Some(reply) = guard_post(&req) {
        return reply;
    }
    let pool = match pool_or_reply().await {
        Ok(pool) => pool,
        Err(reply) => return reply,
    };
    let ip = client_ip(&req);
    let body: Body = match read_json(req).await {
        Ok(body) => body,
        Err(message) => return error_reply(400, message),
    };
    if body.password.chars().count() > auth::MAX_PASSWORD_CHARS {
        return error_reply(400, "Password is too long.");
    }

    let stored_hash = match store::owner_hash(pool).await {
        Ok(Some(hash)) => hash,
        Ok(None) => return error_reply(409, "No password set yet. Complete the first-time setup."),
        Err(e) => return server_error("auth_login", e),
    };
    match store::lock_wait_secs(pool).await {
        Ok(wait) if wait > 0 => {
            store::audit(pool, "login_locked", None, ip.as_deref(), json!({ "wait_secs": wait })).await;
            return rate_limited(wait);
        }
        Ok(_) => {}
        Err(e) => return server_error("auth_login", e),
    }

    let password = body.password;
    let accepted = tokio::task::spawn_blocking(move || auth::verify_password(&stored_hash, &password))
        .await
        .unwrap_or(false);
    if !accepted {
        let wait = store::record_failure(pool).await.unwrap_or(0);
        store::audit(pool, "login_failed", None, ip.as_deref(), json!({ "retry_after_secs": wait })).await;
        return json_reply(401, json!({ "ok": false, "error": "Wrong password.", "retry_after_secs": wait }));
    }
    let _ = store::reset_throttle(pool).await;
    store::audit(pool, "login_ok", None, ip.as_deref(), json!({})).await;
    start_session(pool, json!({ "ok": true })).await
}
