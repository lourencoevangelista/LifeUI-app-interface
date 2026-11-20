use lifeui_api::{
    store,
    util::{Reply, json_reply},
};
use serde_json::{Value, json};
use std::str::FromStr;
use std::time::Duration;
use vercel_runtime::{Error, Request, run, service_fn};

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}

/// Connectivity check, public on purpose so database problems can be diagnosed before login works.
/// It reveals only a coarse stage (env / connect / query / done): never the server version, the host,
/// credentials or error text. Details go to the function logs.
async fn handler(_req: Request) -> Reply {
    let started = std::time::Instant::now();
    let elapsed = || started.elapsed().as_millis() as u64;

    let pool = match store::pool().await {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("db_ping: {e}");
            let stage = if e.starts_with("connect failed") { "connect" } else { "env" };
            // Safe to show: a coarse reason and a hint built from error CLASSES and variable NAMES only, never a value.
            let reason = if e.starts_with("DATABASE_URL is not set") {
                "not_set"
            } else if e.starts_with("invalid DATABASE_URL") {
                "invalid_format"
            } else {
                "connection_failed"
            };
            let hint = e.find(" Hint: ").map(|i| e[i + 1..].to_string());
            // At stage connect, say WHICH network step fails (DNS, TCP). Counts and error classes only: no host, IPs or credentials.
            let network = if stage == "connect" { Some(network_probe().await) } else { None };
            return json_reply(503, json!({ "ok": false, "stage": stage, "reason": reason, "hint": hint, "network": network, "elapsed_ms": elapsed() }));
        }
    };
    match sqlx::query_scalar::<_, i32>("SELECT 1").persistent(false).fetch_one(pool).await {
        Ok(_) => json_reply(200, json!({ "ok": true, "stage": "done", "elapsed_ms": elapsed() })),
        Err(e) => {
            eprintln!("db_ping query failed: {e}");
            json_reply(503, json!({ "ok": false, "stage": "query", "elapsed_ms": elapsed() }))
        }
    }
}

/// Result of one TCP connect attempt: ok, or the error kind and OS error number (e.g. 16 = EBUSY, 101 = no route).
async fn tcp_result(addr: std::net::SocketAddr) -> Value {
    match tokio::time::timeout(Duration::from_secs(4), tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(_)) => json!({ "ok": true }),
        Ok(Err(e)) => json!({ "ok": false, "kind": format!("{:?}", e.kind()), "os_error": e.raw_os_error() }),
        Err(_) => json!({ "ok": false, "kind": "timeout" }),
    }
}

/// Separates the failure into steps so the cause is visible: does DNS work, which IP families come back,
/// can TCP connect to the database host on its port, and can the function reach the internet at all.
async fn network_probe() -> Value {
    let Some((host, port)) = std::env::var("DATABASE_URL")
        .ok()
        .and_then(|url| sqlx::postgres::PgConnectOptions::from_str(&url).ok())
        .map(|o| (o.get_host().to_string(), o.get_port()))
    else {
        return json!({ "error": "no parsable DATABASE_URL" });
    };
    let dns = match tokio::net::lookup_host((host.as_str(), port)).await {
        Ok(addrs) => Ok(addrs.collect::<Vec<_>>()),
        Err(e) => Err(json!({ "kind": format!("{:?}", e.kind()), "os_error": e.raw_os_error() })),
    };
    let (dns_json, tcp) = match dns {
        Ok(addrs) => {
            let v4: Vec<_> = addrs.iter().filter(|a| a.is_ipv4()).copied().collect();
            let v6: Vec<_> = addrs.iter().filter(|a| a.is_ipv6()).copied().collect();
            let mut tcp = Vec::new();
            for (family, list) in [("ipv4", &v4), ("ipv6", &v6)] {
                if let Some(addr) = list.first() {
                    tcp.push(json!({ "family": family, "result": tcp_result(*addr).await }));
                }
            }
            (json!({ "ok": true, "ipv4": v4.len(), "ipv6": v6.len() }), tcp)
        }
        Err(error) => (json!({ "ok": false, "error": error }), Vec::new()),
    };
    // Control: a public IP that answers on 443, to tell "no outbound network" from "database host unreachable".
    let control = tcp_result("1.1.1.1:443".parse().unwrap()).await;
    json!({ "port": port, "dns": dns_json, "tcp": tcp, "control_internet": control })
}
