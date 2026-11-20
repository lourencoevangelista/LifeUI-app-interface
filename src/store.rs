//! PostgreSQL queries behind login. Tables live in schema `lifeui` (see db/src/schema.ts).
//!
//! Every query uses `.persistent(false)`: Supabase's transaction pooler (port 6543) forbids prepared
//! statements. All values are bound parameters, never string-built SQL.

use std::str::FromStr;

use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use tokio::sync::OnceCell;

use crate::auth;

/// Supabase's root CA (db/certs/supabase-prod-ca-2021.crt, "Supabase Root 2021 CA", valid until 2031).
/// Compiled into the function so connections to Supabase are VERIFIED, not merely encrypted:
/// sqlx's `sslmode=require` encrypts but accepts any certificate, so an impersonator could read the traffic.
const SUPABASE_CA: &[u8] = include_bytes!("../db/certs/supabase-prod-ca-2021.crt");

/// Reused across warm invocations of the same function instance.
static POOL: OnceCell<PgPool> = OnceCell::const_new();

/// When DATABASE_URL is missing, point at a look-alike variable (wrong capitalisation is the classic mistake).
/// Only variable NAMES are inspected and printed, never values.
pub fn missing_hint(names: impl Iterator<Item = String>) -> String {
    for name in names {
        if name != "DATABASE_URL" && name.eq_ignore_ascii_case("DATABASE_URL") {
            return format!(" Hint: a variable named '{name}' exists, but names are case-sensitive: rename it to DATABASE_URL.");
        }
    }
    String::new()
}

/// Explains WHAT is wrong with a connection string that does not parse, from the parser's error class only
/// (the value, and above all the password, is never printed).
pub fn invalid_url_hint(parse_error: &str) -> String {
    let hint = if parse_error.contains("invalid domain character") {
        "the host contains characters that are not allowed, typically a leftover placeholder such as <n> or [ ] (use the real host, e.g. aws-0-...pooler.supabase.com)"
    } else if parse_error.contains("relative URL without a base") {
        "the value is not a complete URL: is it wrapped in quotes, or missing the leading postgresql://?"
    } else if parse_error.contains("invalid port number") {
        "the port is not a number: a special character in the password (# / ? @ : %) must be URL-encoded, or the host/port part is damaged"
    } else if parse_error.contains("empty host") {
        "the host part is empty"
    } else {
        "the value is not a valid connection string: copy it again from Supabase, Connect, Transaction pooler, and replace only the password placeholder"
    };
    format!(" Hint: {hint}.")
}

/// Explains WHY a connection failed, from the error class only.
pub fn connect_hint(error: &str) -> String {
    let lower = error.to_lowercase();
    let hint = if lower.contains("password authentication failed") {
        "the password is wrong (did you reset it in Supabase, or leave the [YOUR-PASSWORD] placeholder in?)"
    } else if lower.contains("tenant or user not found") || lower.contains("tenant/user") {
        "wrong project reference or pooler host: check the user (postgres.<project-ref>) and the aws-0 / aws-1 part of the host"
    } else if lower.contains("failed to lookup") || lower.contains("dns") || lower.contains("name or service not known") {
        "the host name does not resolve: check the host part"
    } else if lower.contains("certificate") || lower.contains("tls") {
        "TLS verification failed"
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "the database did not answer in time (is the Supabase project paused?)"
    } else if lower.contains("connection refused") {
        "nothing is listening at that host and port (a connection string without a host falls back to localhost)"
    } else {
        "see the function logs"
    };
    format!(" Hint: {hint}.")
}

/// For a remote host with TLS requested, require a certificate that chains to Supabase's CA and matches
/// the host name (like `sslmode=verify-full`). Local hosts and `sslmode=disable` are left alone (local tests).
fn verify_remote_tls(options: PgConnectOptions) -> PgConnectOptions {
    let local = matches!(options.get_host(), "localhost" | "127.0.0.1" | "::1");
    if local || matches!(options.get_ssl_mode(), PgSslMode::Disable) {
        return options;
    }
    options.ssl_mode(PgSslMode::VerifyFull).ssl_root_cert_from_pem(SUPABASE_CA.to_vec())
}

pub async fn pool() -> Result<&'static PgPool, String> {
    POOL.get_or_try_init(|| async {
        let url = std::env::var("DATABASE_URL").map_err(|_| {
            format!(
                "DATABASE_URL is not set (Supabase shared pooler, transaction mode: port 6543, user postgres.<project-ref>).{}",
                missing_hint(std::env::vars_os().map(|(name, _)| name.to_string_lossy().into_owned()))
            )
        })?;
        // Transaction-mode pooler: no prepared statements, so the statement cache must be off.
        let options = verify_remote_tls(
            PgConnectOptions::from_str(&url)
                .map_err(|e| format!("invalid DATABASE_URL: {e}.{}", invalid_url_hint(&e.to_string())))?
                .statement_cache_capacity(0),
        );
        // One connection per function instance (Supabase's advice for serverless functions).
        PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(8))
            .connect_with(options)
            .await
            .map_err(|e| format!("connect failed: {e}.{}", connect_hint(&e.to_string())))
    })
    .await
}

// ---------------------------------------------------------------- owner password

pub async fn owner_exists(pool: &PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM lifeui.owner_credentials)")
        .persistent(false)
        .fetch_one(pool)
        .await
}

/// Store the hash for the (only) owner. Returns false if a password already exists, so a second
/// setup request can never overwrite the first, even if two arrive at the same moment.
pub async fn create_owner(pool: &PgPool, password_hash: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("INSERT INTO lifeui.owner_credentials (id, password_hash) VALUES (1, $1) ON CONFLICT (id) DO NOTHING")
        .bind(password_hash)
        .persistent(false)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn owner_hash(pool: &PgPool) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT password_hash FROM lifeui.owner_credentials WHERE id = 1")
        .persistent(false)
        .fetch_optional(pool)
        .await
}

// ---------------------------------------------------------------- brute-force throttle

/// Seconds the login is still locked for (0 = not locked).
pub async fn lock_wait_secs(pool: &PgPool) -> Result<i64, sqlx::Error> {
    let wait = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(CEIL(GREATEST(EXTRACT(EPOCH FROM (locked_until - now())), 0))::bigint, 0) \
         FROM lifeui.auth_throttle WHERE id = 1",
    )
    .persistent(false)
    .fetch_optional(pool)
    .await?;
    Ok(wait.unwrap_or(0))
}

/// Count one failed attempt and apply the lockout that the new count calls for.
/// Returns the number of seconds the login is now locked for.
pub async fn record_failure(pool: &PgPool) -> Result<i64, sqlx::Error> {
    let count = sqlx::query_scalar::<_, i32>(
        "INSERT INTO lifeui.auth_throttle (id, failed_count, last_failed_at) VALUES (1, 1, now()) \
         ON CONFLICT (id) DO UPDATE SET failed_count = lifeui.auth_throttle.failed_count + 1, last_failed_at = now() \
         RETURNING failed_count",
    )
    .persistent(false)
    .fetch_one(pool)
    .await?;
    let wait = auth::lockout_seconds(count);
    if wait > 0 {
        sqlx::query("UPDATE lifeui.auth_throttle SET locked_until = now() + make_interval(secs => $1) WHERE id = 1")
            .bind(wait as f64)
            .persistent(false)
            .execute(pool)
            .await?;
    }
    Ok(wait)
}

pub async fn reset_throttle(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE lifeui.auth_throttle SET failed_count = 0, locked_until = NULL WHERE id = 1")
        .persistent(false)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------- sessions (only token hashes are stored)

pub async fn create_session(pool: &PgPool, token_hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM lifeui.sessions WHERE expires_at <= now()")
        .persistent(false)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO lifeui.sessions (token_hash, expires_at) VALUES ($1, now() + make_interval(secs => $2))")
        .bind(token_hash)
        .bind(auth::SESSION_TTL_SECS as f64)
        .persistent(false)
        .execute(pool)
        .await?;
    Ok(())
}

/// True if the token hash belongs to a live session (and marks it as just used).
pub async fn session_valid(pool: &PgPool, token_hash: &str) -> Result<bool, sqlx::Error> {
    let row = sqlx::query_scalar::<_, bool>(
        "UPDATE lifeui.sessions SET last_used_at = now() WHERE token_hash = $1 AND expires_at > now() RETURNING true",
    )
    .bind(token_hash)
    .persistent(false)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

pub async fn delete_session(pool: &PgPool, token_hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM lifeui.sessions WHERE token_hash = $1")
        .bind(token_hash)
        .persistent(false)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------- audit log (append-only, never holds secrets or content)

/// Everything the audit log can record. Mirrors AUDIT_ACTIONS in db/src/schema.ts (a test fails on drift).
pub const AUDIT_ACTIONS: [&str; 19] = [
    "setup_ok",
    "setup_failed",
    "login_ok",
    "login_failed",
    "login_locked",
    "logout",
    "event_create",
    // The Vault (critical information) was removed; its two actions stay so older audit rows remain valid.
    "critical_create",
    "critical_read",
    "agent_inbox_create",
    "agent_inbox_update",
    "agent_switch",
    "agent_token_create",
    "agent_token_revoke",
    "agent_run",
    "agent_stay_awake",
    "news_publish",
    "news_item_update",
    // GET /api/news found no daily briefing for today after 09:45 local time (logged once per day)
    "news_missing",
];

/// An IP address taken from a header, or None if it does not look like one (the value is stored, so it is
/// restricted to hex digits, dots and colons and at most 45 characters, the longest IPv6 text form).
pub fn clean_ip(raw: &str) -> Option<String> {
    let first = raw.split(',').next()?.trim();
    let ok = !first.is_empty() && first.len() <= 45 && first.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':');
    ok.then(|| first.to_string())
}

/// Writes one audit row on an open connection or transaction (used inside a write, so the record and the
/// change commit or roll back together).
pub async fn audit_in(
    conn: &mut sqlx::PgConnection,
    action: &str,
    target: Option<&str>,
    ip: Option<&str>,
    detail: serde_json::Value,
) -> Result<(), sqlx::Error> {
    debug_assert!(AUDIT_ACTIONS.contains(&action), "unknown audit action {action}");
    sqlx::query("INSERT INTO lifeui.audit_log (action, target, ip, detail) VALUES ($1, $2, $3, $4::jsonb)")
        .bind(action)
        .bind(target)
        .bind(ip)
        .bind(detail.to_string())
        .persistent(false)
        .execute(conn)
        .await?;
    Ok(())
}

/// Best-effort audit record for events that are not part of a write (login, logout, reads): a failure to
/// record is logged but never blocks the request.
pub async fn audit(pool: &PgPool, action: &str, target: Option<&str>, ip: Option<&str>, detail: serde_json::Value) {
    let result = match pool.acquire().await {
        Ok(mut conn) => audit_in(&mut conn, action, target, ip, detail).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        eprintln!("audit: could not record '{action}': {e}");
    }
}

/// Most recent audit rows, newest first, optionally one action only. Returns the JSON array as text parsed.
pub async fn list_audit(pool: &PgPool, action: Option<&str>, limit: i64) -> Result<serde_json::Value, sqlx::Error> {
    let text: String = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(t.j ORDER BY t.at DESC, t.id DESC), '[]'::jsonb)::text FROM ( \
           SELECT jsonb_build_object('id', a.id::text, 'at', a.at, 'action', a.action, 'target', a.target, 'ip', a.ip, 'detail', a.detail) AS j, a.at, a.id \
           FROM lifeui.audit_log a WHERE ($1::text IS NULL OR a.action = $1) ORDER BY a.at DESC, a.id DESC LIMIT $2) t",
    )
    .bind(action)
    .bind(limit)
    .persistent(false)
    .fetch_one(pool)
    .await?;
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_addresses_are_cleaned_before_they_are_stored() {
        assert_eq!(clean_ip("203.0.113.7").as_deref(), Some("203.0.113.7"));
        assert_eq!(clean_ip("203.0.113.7, 10.0.0.1").as_deref(), Some("203.0.113.7"), "first hop only");
        assert_eq!(clean_ip("2001:db8::1").as_deref(), Some("2001:db8::1"));
        assert_eq!(clean_ip("'; DROP TABLE x;--"), None);
        assert_eq!(clean_ip("<script>"), None);
        assert_eq!(clean_ip(""), None);
        assert_eq!(clean_ip(&"1".repeat(46)), None);
    }

    #[test]
    fn audit_actions_match_the_drizzle_schema() {
        let path: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "db/src/schema.ts"].iter().collect();
        let schema = std::fs::read_to_string(path).unwrap();
        let start = schema.find("export const AUDIT_ACTIONS = [").expect("AUDIT_ACTIONS not found in schema.ts");
        let body = &schema[start..];
        let listed: Vec<String> = body[body.find('[').unwrap() + 1..body.find(']').unwrap()]
            .split(',')
            .map(|s| s.trim().trim_matches('"').to_string())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(listed, AUDIT_ACTIONS.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "AUDIT_ACTIONS differ from schema.ts");
    }

    fn mode(url: &str) -> PgSslMode {
        verify_remote_tls(PgConnectOptions::from_str(url).unwrap()).get_ssl_mode()
    }

    #[test]
    fn remote_hosts_get_full_verification() {
        let remote = "postgresql://u:p@aws-0-eu-west-2.pooler.supabase.com:6543/postgres";
        assert!(matches!(mode(&format!("{remote}?sslmode=require")), PgSslMode::VerifyFull));
        assert!(matches!(mode(remote), PgSslMode::VerifyFull), "default (prefer) must also be upgraded");
        assert!(matches!(mode(&format!("{remote}?sslmode=verify-ca")), PgSslMode::VerifyFull));
    }

    #[test]
    fn local_and_disabled_connections_are_left_alone() {
        assert!(matches!(mode("postgresql://u@127.0.0.1:5432/db?sslmode=disable"), PgSslMode::Disable));
        assert!(matches!(mode("postgresql://u@localhost:5432/db?sslmode=require"), PgSslMode::Require));
        assert!(matches!(mode("postgresql://u@db.example.com:5432/db?sslmode=disable"), PgSslMode::Disable));
    }

    #[test]
    fn missing_variable_hint_names_look_alikes_only() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter();
        let hint = missing_hint(names(&["PATH", "Database_URL", "POSTGRES_URL"]));
        assert!(hint.contains("'Database_URL'") && hint.contains("case-sensitive"), "{hint}");
        assert!(missing_hint(names(&["database_url"])).contains("'database_url'"));
        assert_eq!(missing_hint(names(&["PATH", "POSTGRES_URL", "DATABASE_URL"])), "", "exact name is not a look-alike");
        assert_eq!(missing_hint(names(&[])), "");
    }

    #[test]
    fn embedded_ca_is_a_pem_certificate() {
        let text = std::str::from_utf8(SUPABASE_CA).unwrap();
        assert!(text.starts_with("-----BEGIN CERTIFICATE-----"));
        assert_eq!(text.matches("BEGIN CERTIFICATE").count(), 1);
    }
}

#[cfg(test)]
mod url_tests {
    use super::*;

    const GOOD: &str = "postgresql://postgres.abc:secretpw@aws-0-eu-west-2.pooler.supabase.com:6543/postgres?sslmode=require";

    fn parse(url: &str) -> Result<PgConnectOptions, String> {
        PgConnectOptions::from_str(url).map_err(|e| e.to_string())
    }

    #[test]
    fn mistakes_that_make_the_value_unparseable_are_explained() {
        // (label, broken value, expected words in the hint). These are the real slip-ups seen or expected.
        let cases = [
            ("placeholder <n> left in the host", GOOD.replace("aws-0-", "aws-<n>-"), "placeholder"),
            ("wrapped in double quotes", format!("\"{GOOD}\""), "quotes"),
            ("special character in the password", GOOD.replace("secretpw", "pa#s/s"), "URL-encoded"),
        ];
        for (label, url, words) in cases {
            let error = parse(&url).err().unwrap_or_else(|| panic!("{label}: expected a parse error"));
            let hint = invalid_url_hint(&error);
            assert!(hint.contains(words), "{label}: hint '{hint}' lacks '{words}' (error: {error})");
            assert!(!hint.contains("secretpw") && !hint.contains("pa#s"), "{label}: the hint must never echo the password");
        }
    }

    #[test]
    fn mistakes_that_parse_fail_later_at_connect_time() {
        // These parse, so on Vercel they show up as stage "connect", not "env".
        for (label, url) in [
            ("password placeholder left in", GOOD.replace("secretpw", "[YOUR-PASSWORD]")),
            ("surrounding whitespace", format!(" {GOOD}\n")),
            ("postgres:// scheme", GOOD.replace("postgresql://", "postgres://")),
            ("session pooler port", GOOD.replace(":6543", ":5432")),
        ] {
            assert!(parse(&url).is_ok(), "{label} should parse");
        }
        // a value without a scheme silently falls back to localhost: the hint must explain "connection refused"
        assert_eq!(parse(&GOOD.replace("postgresql://", "")).unwrap().get_host(), "localhost");
        assert!(connect_hint("error communicating with database: Connection refused (os error 111)").contains("localhost"));
    }

    #[test]
    fn connect_failures_are_classified() {
        for (error, words) in [
            ("error returned from database: password authentication failed for user \"postgres.abc\"", "password is wrong"),
            ("error returned from database: (ENOTFOUND) tenant/user postgres.abc not found", "project reference"),
            ("failed to lookup address information: Name or service not known", "does not resolve"),
            ("invalid peer certificate: UnknownIssuer", "TLS verification"),
            ("pool timed out while waiting for an open connection", "did not answer"),
            ("something unexpected", "function logs"),
        ] {
            assert!(connect_hint(error).contains(words), "'{error}' -> '{}'", connect_hint(error));
        }
    }
}
