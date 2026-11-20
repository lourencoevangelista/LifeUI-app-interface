//! Shared code for the Vercel Rust functions in `api/`.
//!
//! - `auth`  : pure logic (password hashing, session tokens, cookies, lockout policy). No I/O, unit tested.
//! - `domain`: validation rules for events (mirror the SHACL shapes and DB checks).
//! - `shapes`: the authoritative gate: validates a write against the real SHACL shapes file (rudof engine).
//! - `store` : the PostgreSQL queries (login: owner password hash, throttle, sessions; data: events).
//! - `agent`: the Agent tab's work queue (inbox), message feed and run log.
//! - `news` : the News tab: briefings published by the home server, read/save/dismiss, "stay awake tonight".
//! - `util`  : HTTP helpers (JSON replies, request guards, body parsing, shared connection pool).

pub mod agent;
pub mod auth;
pub mod data;
pub mod domain;
pub mod news;
pub mod shapes;
pub mod store;
pub mod util;
