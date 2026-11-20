use lifeui_api::{
    auth, store,
    util::{Reply, client_ip, guard_post, json_reply_with_cookie, pool_or_reply, session_token},
};
use serde_json::json;
use vercel_runtime::{Error, Request, run, service_fn};

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}

/// Ends the session: deletes it in the database and tells the browser to drop the cookie.
async fn handler(req: Request) -> Reply {
    if let Some(reply) = guard_post(&req) {
        return reply;
    }
    if let Some(token) = session_token(&req) {
        if let Ok(pool) = pool_or_reply().await {
            let hash = auth::hash_token(&token);
            // only a live session counts as a logout in the audit trail (a forged cookie is not an event)
            let was_live = store::session_valid(pool, &hash).await.unwrap_or(false);
            let _ = store::delete_session(pool, &hash).await;
            if was_live {
                store::audit(pool, "logout", None, client_ip(&req).as_deref(), json!({})).await;
            }
        }
    }
    json_reply_with_cookie(200, json!({ "ok": true }), auth::clear_cookie())
}
