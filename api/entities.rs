use lifeui_api::{
    data::{self, EntityFilter},
    domain::ID_BASE,
    util::{Reply, error_reply, json_reply, method_not_allowed, pool_or_reply, query_params, require_auth, server_error, validation_reply},
};
use serde_json::json;
use vercel_runtime::{Error, Request, run, service_fn};

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}

/// The knowledge base outside events (documents, projects, tasks, courses, people, tools...).
/// Read-only; login required.
///   GET /api/entities                        list sorted by class then name, plus class counts.
///                                            Filters: class=<ClassName>, q=<part of the name>, limit (1-200, default 100)
///   GET /api/entities?iri=<entity iri>       one entity with its properties and relations in both directions
async fn handler(req: Request) -> Reply {
    let pool = match pool_or_reply().await {
        Ok(pool) => pool,
        Err(reply) => return reply,
    };
    if let Err(reply) = require_auth(&req, pool).await {
        return reply;
    }
    if req.method().as_str() != "GET" {
        return method_not_allowed("GET");
    }
    let params = query_params(&req);

    if let Some(iri) = params.get("iri") {
        if !iri.starts_with(ID_BASE) || iri.chars().count() > 300 || iri.chars().any(char::is_whitespace) {
            return error_reply(400, "iri must be an entity identifier of this knowledge base.");
        }
        return match data::get_entity(pool, iri).await {
            Ok(Some(found)) => json_reply(
                200,
                json!({ "ok": true, "entity": found["entity"], "links_out": found["links_out"], "links_in": found["links_in"] }),
            ),
            Ok(None) => error_reply(404, "No such entity (events are in the Timeline)."),
            Err(e) => server_error("entity get", e),
        };
    }

    let mut problems = Vec::new();
    let class = params.get("class").cloned().filter(|c| !c.is_empty());
    if class.as_deref().is_some_and(|c| c.len() > 60 || !c.chars().all(|ch| ch.is_ascii_alphanumeric())) {
        problems.push("class must be a class name such as Task or Course.".into());
    }
    let q = params.get("q").map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
    if q.as_deref().is_some_and(|q| q.chars().count() > 100) {
        problems.push("q must have at most 100 characters.".into());
    }
    let limit = match params.get("limit") {
        None => 100,
        Some(text) => match text.parse::<i64>() {
            Ok(n) if (1..=200).contains(&n) => n,
            _ => {
                problems.push("limit must be a whole number from 1 to 200.".into());
                100
            }
        },
    };
    if !problems.is_empty() {
        return validation_reply(problems);
    }
    match data::list_entities(pool, &EntityFilter { class, q, limit }).await {
        Ok(found) => {
            let count = found["entities"].as_array().map_or(0, Vec::len);
            json_reply(200, json!({ "ok": true, "count": count, "entities": found["entities"], "classes": found["classes"] }))
        }
        Err(e) => server_error("entity list", e),
    }
}
