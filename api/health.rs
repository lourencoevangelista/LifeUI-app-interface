use serde_json::{Value, json};
use vercel_runtime::{Error, Request, run, service_fn};

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}

/// Liveness probe: proves the Rust runtime works, touches no database.
async fn handler(_req: Request) -> Result<Value, Error> {
    Ok(json!({
        "ok": true,
        "service": "lifeui-api",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
