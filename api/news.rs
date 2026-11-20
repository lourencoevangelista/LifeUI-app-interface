use lifeui_api::{
    news::{self, NewsWrite},
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

const MAX_BODY: usize = 4 * 1024;

/// The News tab. Login required for everything. (The home server PUBLISHES through POST /api/runner/news.)
///   GET  /api/news[?date=YYYY-MM-DD][&type=daily|weekly]   that briefing (default: the newest), the dates that have
///                                                           one, the "stay awake tonight" setting and `today` (Today card:
///                                                           open inbox items, LifeUI events today, missing briefing)
///   POST /api/news {"kind":"item","id":"<uuid>","action":"read|unread|save|unsave|dismiss|undismiss"}
///   POST /api/news {"kind":"stay_awake","on":true|false}   keep the home server on tonight (skips its 21:00 shutdown)
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
    let date = params.get("date").map(String::as_str).filter(|d| !d.is_empty());
    if let Some(d) = date {
        let b = d.as_bytes();
        let shaped = b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
        if !shaped {
            problems.push("date must be YYYY-MM-DD.".to_string());
        }
    }
    let kind = params.get("type").map(String::as_str).unwrap_or("daily");
    if !news::BRIEFING_TYPES.contains(&kind) {
        problems.push(format!("type must be one of: {}.", news::BRIEFING_TYPES.join(", ")));
    }
    if !problems.is_empty() {
        return validation_reply(problems);
    }
    let briefing = match news::briefing(pool, date, kind).await {
        Ok(b) => b,
        Err(e) => return server_error("news briefing", e),
    };
    let dates = match news::dates(pool, kind).await {
        Ok(d) => d,
        Err(e) => return server_error("news dates", e),
    };
    let stay = match news::stay_awake(pool).await {
        Ok(s) => s,
        Err(e) => return server_error("news stay awake", e),
    };
    let today = match news::today(pool, client_ip(req).as_deref()).await {
        Ok(t) => t,
        Err(e) => return server_error("news today", e),
    };
    json_reply(200, json!({ "ok": true, "briefing": briefing, "dates": dates, "stay_awake": stay, "today": today }))
}

async fn write(req: Request, pool: &PgPool) -> Reply {
    if let Some(reply) = check_origin(&req) {
        return reply;
    }
    let ip = client_ip(&req);
    let input: NewsWrite = match read_json_max(req, MAX_BODY).await {
        Ok(input) => input,
        Err(message) => return error_reply(400, message),
    };
    match input {
        NewsWrite::Item { id, action } => {
            if let Err(problems) = news::validate_item_action(&id, &action) {
                return validation_reply(problems);
            }
            match news::item_action(pool, &id, &action, ip.as_deref()).await {
                Ok(Some(item)) => json_reply(200, json!({ "ok": true, "item": item })),
                Ok(None) => error_reply(404, "No such news item."),
                Err(e) => server_error("news item", e),
            }
        }
        NewsWrite::StayAwake { on } => match news::set_stay_awake(pool, on, ip.as_deref()).await {
            Ok(stay) => json_reply(200, json!({ "ok": true, "stay_awake": stay })),
            Err(e) => server_error("news stay awake", e),
        },
    }
}
