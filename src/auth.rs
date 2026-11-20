//! Pure authentication logic: no database, no HTTP, fully unit tested.
//!
//! Security model (single owner):
//!  - the password is stored ONLY as an Argon2id hash (PHC string with its own random salt);
//!  - a session is a random 256-bit token; the browser keeps the token in an HttpOnly cookie and the
//!    database keeps only the SHA-256 of the token, so a database leak does not reveal live sessions;
//!  - failed logins lock the account for an exponentially growing time (see `lockout_seconds`).

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// `__Host-` prefix: the browser only accepts the cookie if it is Secure, has Path=/ and no Domain.
pub const COOKIE_NAME: &str = "__Host-lifeui_session";
pub const SESSION_TTL_SECS: i64 = 30 * 24 * 60 * 60;
pub const MIN_PASSWORD_CHARS: usize = 12;
pub const MAX_PASSWORD_CHARS: usize = 200;

/// OWASP-recommended minimum for Argon2id: 19 MiB memory, 2 iterations, 1 lane.
fn production_params() -> Params {
    Params::new(19 * 1024, 2, 1, None).expect("valid argon2 parameters")
}

fn hasher(params: Params) -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Hash a password into a PHC string (`$argon2id$v=19$m=...,t=...,p=...$salt$hash`) with a random salt.
pub fn hash_password(password: &str) -> Result<String, String> {
    hash_with(production_params(), password)
}

fn hash_with(params: Params, password: &str) -> Result<String, String> {
    hasher(params)
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| format!("hashing failed: {e}"))
}

/// Verify a password against a stored PHC string. The parameters stored in the hash are used, so
/// they can be raised later without breaking existing hashes. Any malformed hash is simply "no".
pub fn verify_password(stored_hash: &str, password: &str) -> bool {
    match PasswordHash::new(stored_hash) {
        Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

/// Rules for a NEW password. Returns a message safe to show to the user.
pub fn validate_new_password(password: &str) -> Result<(), &'static str> {
    let chars = password.chars().count();
    if chars < MIN_PASSWORD_CHARS {
        return Err("Password must have at least 12 characters (a passphrase of 4 or more random words is best).");
    }
    if chars > MAX_PASSWORD_CHARS {
        return Err("Password is too long (maximum 200 characters).");
    }
    if password.chars().all(|c| c == password.chars().next().unwrap_or(' ')) {
        return Err("Password must not be one repeated character.");
    }
    Ok(())
}

/// A fresh random session token (32 bytes from the operating system, URL-safe base64).
pub fn new_session_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("no randomness available: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// What the database stores for a token: lowercase hex SHA-256.
pub fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Compare two secrets without leaking, through timing, how many leading bytes matched.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Seconds to lock the login after `failed_count` consecutive failures:
/// 1-4 failures: none (Argon2 already makes each guess slow); 5th: 30 s, then doubling, capped at 15 min.
pub fn lockout_seconds(failed_count: i32) -> i64 {
    if failed_count < 5 {
        return 0;
    }
    let doublings = (failed_count - 5).min(10) as u32;
    (30i64 * (1i64 << doublings)).min(15 * 60)
}

pub fn session_cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={SESSION_TTL_SECS}")
}

pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0")
}

/// Extract one cookie value from a `Cookie:` request header.
pub fn parse_cookie(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|pair| {
        let (k, v) = pair.trim().split_once('=')?;
        (k == name && !v.is_empty()).then(|| v.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cheap() -> Params {
        Params::new(8, 1, 1, None).unwrap() // fast parameters, only for tests of the plumbing
    }

    #[test]
    fn hash_roundtrip_and_wrong_password() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "unexpected PHC prefix: {hash}");
        assert!(verify_password(&hash, "correct horse battery staple"));
        assert!(!verify_password(&hash, "correct horse battery stapl"));
        assert!(!verify_password(&hash, ""));
    }

    #[test]
    fn same_password_gives_different_hashes_because_of_salt() {
        let a = hash_with(cheap(), "same password 123").unwrap();
        let b = hash_with(cheap(), "same password 123").unwrap();
        assert_ne!(a, b);
        assert!(verify_password(&a, "same password 123") && verify_password(&b, "same password 123"));
    }

    #[test]
    fn hash_never_contains_the_password() {
        let hash = hash_with(cheap(), "my secret passphrase").unwrap();
        assert!(!hash.contains("my secret passphrase"));
    }

    #[test]
    fn malformed_hash_is_rejected_not_panicking() {
        assert!(!verify_password("", "x"));
        assert!(!verify_password("not a phc string", "x"));
        assert!(!verify_password("$argon2id$v=19$broken", "x"));
    }

    #[test]
    fn password_policy() {
        assert!(validate_new_password("short").is_err());
        assert!(validate_new_password("elevenchars").is_err()); // 11
        assert!(validate_new_password("twelve chars").is_ok()); // 12 with a space
        assert!(validate_new_password("aaaaaaaaaaaaaaaa").is_err());
        assert!(validate_new_password(&"x1".repeat(101)).is_err()); // 202 chars
        assert!(validate_new_password("pãssphrase çom acentos").is_ok()); // counts characters, not bytes
    }

    #[test]
    fn session_tokens_are_unique_long_and_url_safe() {
        let a = new_session_token().unwrap();
        let b = new_session_token().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43); // 32 bytes -> 43 base64url chars without padding
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn token_hash_is_stable_hex_sha256() {
        // Known SHA-256 of "abc"
        assert_eq!(hash_token("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hash_token("abc").len(), 64);
        assert_ne!(hash_token("abc"), hash_token("abd"));
    }

    #[test]
    fn constant_time_eq_behaves_like_eq() {
        assert!(constant_time_eq("setup-code-1", "setup-code-1"));
        assert!(!constant_time_eq("setup-code-1", "setup-code-2"));
        assert!(!constant_time_eq("short", "longer value"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn lockout_schedule() {
        let schedule: Vec<i64> = (0..=12).map(lockout_seconds).collect();
        assert_eq!(schedule, vec![0, 0, 0, 0, 0, 30, 60, 120, 240, 480, 900, 900, 900]);
        assert_eq!(lockout_seconds(i32::MAX), 900); // never overflows
        assert_eq!(lockout_seconds(-3), 0);
    }

    #[test]
    fn cookie_building() {
        let c = session_cookie("TOKEN");
        for needed in ["__Host-lifeui_session=TOKEN", "Path=/", "HttpOnly", "Secure", "SameSite=Strict", "Max-Age=2592000"] {
            assert!(c.contains(needed), "{c} lacks {needed}");
        }
        assert!(!c.contains("Domain"), "a __Host- cookie must not set Domain");
        assert!(clear_cookie().contains("Max-Age=0"));
    }

    #[test]
    fn cookie_parsing() {
        let header = "theme=dark; __Host-lifeui_session=abc123; other=1";
        assert_eq!(parse_cookie(header, COOKIE_NAME).as_deref(), Some("abc123"));
        assert_eq!(parse_cookie("__Host-lifeui_session=", COOKIE_NAME), None);
        assert_eq!(parse_cookie("x=1", COOKIE_NAME), None);
        assert_eq!(parse_cookie("", COOKIE_NAME), None);
        // a cookie whose name merely ends with ours must not match
        assert_eq!(parse_cookie("evil__Host-lifeui_session=zzz", COOKIE_NAME), None);
    }
}
