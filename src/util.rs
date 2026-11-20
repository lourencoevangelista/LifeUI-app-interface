//! HTTP helpers shared by the functions in `api/`.

use http_body_util::{BodyExt, Limited};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::PgPool;
use vercel_runtime::{Error, Request, Response};

use crate::{auth, store};

pub type Reply = Result<Response<Value>, Error>;

/// Largest request body we accept (login bodies are tiny).
const MAX_BODY_BYTES: usize = 4096;

fn build(status: u16, body: Value, extra_header: Option<(&str, String)>) -> Reply {
    let mut builder = Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .header("x-content-type-options", "nosniff");
    if let Some((name, value)) = extra_header {
        builder = builder.header(name, value);
    }
    Ok(builder.body(body)?)
}

pub fn json_reply(status: u16, body: Value) -> Reply {
    build(status, body, None)
}

pub fn json_reply_with_cookie(status: u16, body: Value, cookie: String) -> Reply {
    build(status, body, Some(("set-cookie", cookie)))
}

pub fn error_reply(status: u16, message: &str) -> Reply {
    json_reply(status, json!({ "ok": false, "error": message }))
}

/// 429 with a `Retry-After` header: the login is locked after repeated failures.
pub fn rate_limited(wait_secs: i64) -> Reply {
    build(
        429,
        json!({ "ok": false, "error": "Too many failed attempts. Try again later.", "retry_after_secs": wait_secs }),
        Some(("retry-after", wait_secs.to_string())),
    )
}

/// For problems that are ours, not the caller's: log the detail, tell the caller nothing sensitive.
pub fn server_error(context: &str, detail: impl std::fmt::Display) -> Reply {
    eprintln!("server error in {context}: {detail}");
    error_reply(500, "Server error. Check the function logs.")
}

/// Shared connection pool, or a ready-made 503 reply when the database is not configured/reachable.
pub async fn pool_or_reply() -> Result<&'static PgPool, Reply> {
    store::pool().await.map_err(|e| {
        eprintln!("database unavailable: {e}");
        error_reply(503, "Database unavailable.")
    })
}

pub fn method_not_allowed(allow: &str) -> Reply {
    Response::builder()
        .status(405)
        .header("allow", allow)
        .header("content-type", "application/json")
        .body(json!({ "ok": false, "error": format!("Allowed methods: {allow}.") }))
        .map_err(Error::from)
}

/// Basic CSRF defence for requests that change data: if the browser sent an `Origin`, it must match `Host`.
/// Returns Some(reply) when the request must be rejected.
pub fn check_origin(req: &Request) -> Option<Reply> {
    let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok());
    if let (Some(origin), Some(host)) = (header("origin"), header("host")) {
        let origin_host = origin.split_once("://").map(|(_, rest)| rest).unwrap_or(origin);
        if origin_host != host {
            return Some(error_reply(403, "Cross-site request refused."));
        }
    }
    None
}

/// POST-only guard (method plus origin check). Returns Some(reply) when the request must be rejected.
pub fn guard_post(req: &Request) -> Option<Reply> {
    if req.method() != http::Method::POST {
        return Some(method_not_allowed("POST"));
    }
    check_origin(req)
}

/// Read and parse a small JSON body (login endpoints).
pub async fn read_json<T: DeserializeOwned>(req: Request) -> Result<T, &'static str> {
    read_json_max(req, MAX_BODY_BYTES).await
}

/// Read and parse a JSON body of at most `max_bytes`.
pub async fn read_json_max<T: DeserializeOwned>(req: Request, max_bytes: usize) -> Result<T, &'static str> {
    let body = Limited::new(req.into_body(), max_bytes);
    let bytes = body.collect().await.map_err(|_| "Request body too large or unreadable.")?.to_bytes();
    serde_json::from_slice(&bytes).map_err(|_| "Request body must be valid JSON with the expected fields.")
}

/// Every data endpoint calls this first: 401 unless the request carries a live session.
pub async fn require_auth(req: &Request, pool: &PgPool) -> Result<(), Reply> {
    if is_authenticated(req, pool).await {
        Ok(())
    } else {
        Err(error_reply(401, "Login required."))
    }
}

/// Validation failure: 400 with every problem listed.
pub fn validation_reply(problems: Vec<String>) -> Reply {
    json_reply(400, json!({ "ok": false, "error": "Validation failed.", "details": problems }))
}

/// Query-string parameters of the request (percent-decoded; the last value of a repeated key wins).
pub fn query_params(req: &Request) -> std::collections::HashMap<String, String> {
    parse_query(req.uri().query().unwrap_or(""))
}

pub fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() && bytes[i + 1].is_ascii_hexdigit() && bytes[i + 2].is_ascii_hexdigit() => {
                out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap_or(b'?'));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parsing_and_percent_decoding() {
        let q = parse_query("class=Milestone&from=2026-01-01T10%3A00%3A00Z&limit=5&flag&name=a+b%20c&limit=7");
        assert_eq!(q.get("class").map(String::as_str), Some("Milestone"));
        assert_eq!(q.get("from").map(String::as_str), Some("2026-01-01T10:00:00Z"));
        assert_eq!(q.get("limit").map(String::as_str), Some("7"), "last value wins");
        assert_eq!(q.get("flag").map(String::as_str), Some(""));
        assert_eq!(q.get("name").map(String::as_str), Some("a b c"));
        assert!(parse_query("").is_empty());
    }

    #[test]
    fn percent_decoding_is_forgiving_about_bad_input() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("%41%42"), "AB");
        assert_eq!(percent_decode("%C3%A7"), "ç");
    }
}

/// The caller's IP address for the audit log (Vercel sets x-forwarded-for; only a well-formed address is kept).
pub fn client_ip(req: &Request) -> Option<String> {
    let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok()).and_then(store::clean_ip);
    header("x-forwarded-for").or_else(|| header("x-real-ip"))
}

/// The raw session token from the request cookie, if any.
pub fn session_token(req: &Request) -> Option<String> {
    let cookie = req.headers().get("cookie")?.to_str().ok()?;
    auth::parse_cookie(cookie, auth::COOKIE_NAME)
}

/// The token from an `Authorization: Bearer <token>` header (used by the scheduled agent, never by the browser).
pub fn bearer_token(req: &Request) -> Option<String> {
    let value = req.headers().get("authorization")?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?.trim();
    (!token.is_empty() && token.len() <= 200).then(|| token.to_string())
}

/// Is this request carrying a live session?
pub async fn is_authenticated(req: &Request, pool: &PgPool) -> bool {
    match session_token(req) {
        Some(token) => store::session_valid(pool, &auth::hash_token(&token)).await.unwrap_or(false),
        None => false,
    }
}

/// Create a session for the owner and answer with the cookie.
pub async fn start_session(pool: &PgPool, body: Value) -> Reply {
    let token = match auth::new_session_token() {
        Ok(t) => t,
        Err(e) => return server_error("start_session", e),
    };
    if let Err(e) = store::create_session(pool, &auth::hash_token(&token)).await {
        return server_error("start_session", e);
    }
    json_reply_with_cookie(200, body, auth::session_cookie(&token))
}
