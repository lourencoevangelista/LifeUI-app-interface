use lifeui_api::{
    store,
    util::{Reply, is_authenticated, json_reply, pool_or_reply, server_error},
};
use serde_json::json;
use vercel_runtime::{Error, Request, run, service_fn};

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}

/// Tells the frontend which screen to show: first-time setup, login, or the app.
async fn handler(req: Request) -> Reply {
    let pool = match pool_or_reply().await {
        Ok(pool) => pool,
        Err(reply) => return reply,
    };
    let setup_required = match store::owner_exists(pool).await {
        Ok(exists) => !exists,
        Err(e) => return server_error("auth_status", e),
    };
    let authenticated = is_authenticated(&req, pool).await;
    json_reply(200, json!({ "ok": true, "setup_required": setup_required, "authenticated": authenticated }))
}
