//! Domain rules for the persisted ontology class: events (and its subclasses).
//!
//! Pure functions, no I/O, unit tested. They mirror three other places that must agree:
//!  - the SHACL shapes (ontology/lifeui.shapes.ttl): EventShape, TransactionShape, MeasurementShape;
//!  - the CHECK constraints in db/src/schema.ts (the database is the last line of defence);
//!  - the controlled vocabularies in ontology/lifeui.ttl.
//! Tests at the bottom read those files and fail if the lists here drift from them.

use serde::Deserialize;
use serde_json::{Map, Value};
use time::{Date, OffsetDateTime, format_description::well_known::{Iso8601, Rfc3339}};

pub const ONTOLOGY_BASE: &str = "https://lifeui.example/ontology#";
pub const ID_BASE: &str = "https://lifeui.example/id/";

/// life:Event and every subclass that is stored in the events table.
pub const EVENT_CLASSES: [&str; 9] =
    ["Event", "Appointment", "Meeting", "Milestone", "Trip", "Workout", "Measurement", "Transaction", "Award"];
pub const STATUSES: [&str; 5] = ["Planned", "InProgress", "Completed", "Cancelled", "Blocked"];
pub const SENSITIVITIES: [&str; 4] = ["Public", "Personal", "Sensitive", "Secret"];

pub const MAX_NAME_CHARS: usize = 200;
pub const MAX_DESCRIPTION_CHARS: usize = 2000;
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024;

/// Payload keys that would hold a secret VALUE. Secrets are never stored in LifeUI, so such keys are refused
/// anywhere in a payload.
const FORBIDDEN_PAYLOAD_KEYS: [&str; 11] =
    ["password", "passwd", "passphrase", "secret", "token", "api_key", "apikey", "private_key", "pin", "cvv", "cvc"];

// ------------------------------------------------------------------------------------ events

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventInput {
    pub class: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub occurred_at: Option<String>,
    pub occurred_on: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub sensitivity: Option<String>,
    pub confidence: Option<f64>,
    pub payload: Option<Value>,
    /// id of the event this one replaces (a new version; the old row is kept and linked)
    pub supersedes: Option<String>,
    /// id of the Agent inbox item whose PROPOSED event this is: the owner pressed Add (or edited and saved it). The
    /// event and the "added" mark on the proposal are written in one transaction, so a proposal is added only once.
    #[serde(default)]
    pub from_proposal: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidEvent {
    pub class_iri: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub occurred_at: Option<String>,
    pub occurred_on: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub sensitivity: String,
    pub confidence: f64,
    pub payload: Value,
    pub supersedes: Option<String>,
    pub from_proposal: Option<String>,
}

/// Validate everything and return ALL problems at once (friendlier than one error per round trip).
pub fn validate_event(input: EventInput) -> Result<ValidEvent, Vec<String>> {
    let mut errors = Vec::new();

    let class = input.class.as_deref().unwrap_or("Event");
    if !EVENT_CLASSES.contains(&class) {
        errors.push(format!("class must be one of: {}.", EVENT_CLASSES.join(", ")));
    }
    let name = clean_text("name", &input.name, 1, MAX_NAME_CHARS, &mut errors);
    let description = optional_text("description", input.description.as_deref(), MAX_DESCRIPTION_CHARS, &mut errors);
    if !STATUSES.contains(&input.status.as_str()) {
        errors.push(format!("status must be one of: {}.", STATUSES.join(", ")));
    }

    // SHACL EventShape: at least one of occurred_at / occurred_on / started_at
    if input.occurred_at.is_none() && input.occurred_on.is_none() && input.started_at.is_none() {
        errors.push("An event needs occurred_at, occurred_on or started_at.".into());
    }
    let occurred_at = parse_instant("occurred_at", input.occurred_at, &mut errors);
    let started_at = parse_instant("started_at", input.started_at, &mut errors);
    let ended_at = parse_instant("ended_at", input.ended_at, &mut errors);
    let occurred_on = parse_date("occurred_on", input.occurred_on, &mut errors);
    if let (Some((_, start)), Some((_, end))) = (&started_at, &ended_at) {
        if start > end {
            errors.push("started_at must not be after ended_at.".into());
        }
    }

    let sensitivity = input.sensitivity.unwrap_or_else(|| "Personal".into());
    if !SENSITIVITIES.contains(&sensitivity.as_str()) {
        errors.push(format!("sensitivity must be one of: {}.", SENSITIVITIES.join(", ")));
    }
    let confidence = validate_confidence(input.confidence, &mut errors);
    let payload = validate_payload(input.payload, &mut errors);
    if let Some(id) = &input.supersedes {
        if !is_uuid(id) {
            errors.push("supersedes must be the id (UUID) of an existing event.".into());
        }
    }
    if let Some(id) = &input.from_proposal {
        if !is_uuid(id) {
            errors.push("from_proposal must be the id (UUID) of an Agent inbox item.".into());
        }
        if input.supersedes.is_some() {
            errors.push("A proposed event is a new event: it cannot also supersede one.".into());
        }
    }

    // class-specific SHACL shapes (stored in the payload)
    if errors.is_empty() || payload.is_object() {
        match class {
            "Transaction" => {
                if !payload.get("amount").is_some_and(Value::is_number) {
                    errors.push("A Transaction needs payload.amount (a number; negative for money out).".into());
                }
                if !payload.get("currency").and_then(Value::as_str).is_some_and(is_currency) {
                    errors.push("A Transaction needs payload.currency as an ISO 4217 code such as EUR.".into());
                }
            }
            "Measurement" => {
                for key in ["metric", "unit"] {
                    if !payload.get(key).and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty()) {
                        errors.push(format!("A Measurement needs payload.{key} (non-empty text)."));
                    }
                }
                if !payload.get("value").is_some_and(Value::is_number) {
                    errors.push("A Measurement needs payload.value (a number).".into());
                }
            }
            _ => {}
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ValidEvent {
        class_iri: format!("{ONTOLOGY_BASE}{class}"),
        name,
        description,
        status: input.status,
        occurred_at: occurred_at.map(|(raw, _)| raw),
        occurred_on: occurred_on.map(|(raw, _)| raw),
        started_at: started_at.map(|(raw, _)| raw),
        ended_at: ended_at.map(|(raw, _)| raw),
        sensitivity,
        confidence,
        payload,
        supersedes: input.supersedes,
        from_proposal: input.from_proposal,
    })
}

// ------------------------------------------------------------------------------------ small validators

fn clean_text(field: &str, raw: &str, min: usize, max: usize, errors: &mut Vec<String>) -> String {
    let text = raw.trim();
    let chars = text.chars().count();
    if chars < min || chars > max {
        errors.push(format!("{field} must have between {min} and {max} characters."));
    }
    text.to_string()
}

fn optional_text(field: &str, raw: Option<&str>, max: usize, errors: &mut Vec<String>) -> Option<String> {
    let text = raw?.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() > max {
        errors.push(format!("{field} is too long (maximum {max} characters)."));
    }
    Some(text.to_string())
}

fn validate_confidence(value: Option<f64>, errors: &mut Vec<String>) -> f64 {
    match value {
        None => 1.0, // entered by the owner in the app
        Some(c) if c.is_finite() && (0.0..=1.0).contains(&c) => c,
        Some(_) => {
            errors.push("confidence must be a number between 0 and 1.".into());
            1.0
        }
    }
}

fn validate_payload(value: Option<Value>, errors: &mut Vec<String>) -> Value {
    let payload = match value {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(v @ Value::Object(_)) => v,
        Some(_) => {
            errors.push("payload must be a JSON object.".into());
            return Value::Object(Map::new());
        }
    };
    if payload.to_string().len() > MAX_PAYLOAD_BYTES {
        errors.push(format!("payload is too large (maximum {} KB).", MAX_PAYLOAD_BYTES / 1024));
    }
    if let Some(key) = find_forbidden_key(&payload) {
        errors.push(format!(
            "payload must not contain a '{key}' field: secrets are never stored."
        ));
    }
    payload
}

fn find_forbidden_key(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => map.iter().find_map(|(key, inner)| {
            if FORBIDDEN_PAYLOAD_KEYS.contains(&key.to_lowercase().as_str()) {
                Some(key.clone())
            } else {
                find_forbidden_key(inner)
            }
        }),
        Value::Array(items) => items.iter().find_map(find_forbidden_key),
        _ => None,
    }
}

/// Returns the original text (for SQL) plus the parsed value (for comparisons).
fn parse_instant(field: &str, raw: Option<String>, errors: &mut Vec<String>) -> Option<(String, OffsetDateTime)> {
    let text = raw?;
    match OffsetDateTime::parse(&text, &Rfc3339) {
        Ok(parsed) => Some((text, parsed)),
        Err(_) => {
            errors.push(format!("{field} must be an RFC 3339 timestamp such as 2026-05-29T10:00:00Z."));
            None
        }
    }
}

fn parse_date(field: &str, raw: Option<String>, errors: &mut Vec<String>) -> Option<(String, Date)> {
    let text = raw?;
    match Date::parse(&text, &Iso8601::DATE) {
        Ok(parsed) => Some((text, parsed)),
        Err(_) => {
            errors.push(format!("{field} must be a date such as 2026-05-29."));
            None
        }
    }
}

/// A date (2026-05-29) or an RFC 3339 instant: the two forms accepted by the `from` / `to` timeline filters.
pub fn is_date_or_instant(text: &str) -> bool {
    OffsetDateTime::parse(text, &Rfc3339).is_ok() || Date::parse(text, &Iso8601::DATE).is_ok()
}

pub fn is_currency(text: &str) -> bool {
    text.len() == 3 && text.bytes().all(|b| b.is_ascii_uppercase())
}

/// 8-4-4-4-12 hexadecimal groups.
pub fn is_uuid(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { *c == b'-' } else { c.is_ascii_hexdigit() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_proposed_event_names_its_inbox_item_and_is_never_a_new_version() {
        let base = json!({"name": "Dentist", "status": "Planned", "occurred_on": "2026-11-27"});
        let with = |extra: Value| {
            let mut v = base.clone();
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            validate_event(serde_json::from_value::<EventInput>(v).unwrap())
        };
        let ok = with(json!({"from_proposal": "123e4567-e89b-12d3-a456-426614174000"})).unwrap();
        assert_eq!(ok.from_proposal.as_deref(), Some("123e4567-e89b-12d3-a456-426614174000"));
        assert!(with(json!({"from_proposal": "nope"})).is_err());
        assert!(with(json!({"from_proposal": "123e4567-e89b-12d3-a456-426614174000", "supersedes": "123e4567-e89b-12d3-a456-426614174001"})).is_err());
    }
    use std::path::PathBuf;

    fn event(value: Value) -> Result<ValidEvent, Vec<String>> {
        validate_event(serde_json::from_value(value).expect("shape of EventInput"))
    }
    fn errors_of<T: std::fmt::Debug>(result: Result<T, Vec<String>>) -> String {
        result.expect_err("expected validation errors").join(" | ")
    }

    // ---- events
    #[test]
    fn minimal_valid_event_gets_defaults() {
        let e = event(json!({"name": "  Project kick-off  ", "status": "Completed", "occurred_on": "2026-11-08"})).unwrap();
        assert_eq!(e.class_iri, "https://lifeui.example/ontology#Event");
        assert_eq!(e.name, "Project kick-off");
        assert_eq!(e.sensitivity, "Personal");
        assert_eq!(e.confidence, 1.0);
        assert_eq!(e.payload, json!({}));
    }

    #[test]
    fn event_needs_some_time() {
        let msg = errors_of(event(json!({"name": "x", "status": "Planned"})));
        assert!(msg.contains("needs occurred_at, occurred_on or started_at"), "{msg}");
        assert!(event(json!({"name": "x", "status": "Planned", "started_at": "2026-01-01T10:00:00Z"})).is_ok());
    }

    #[test]
    fn event_time_formats_and_order() {
        assert!(errors_of(event(json!({"name": "x", "status": "Planned", "occurred_at": "yesterday"}))).contains("RFC 3339"));
        assert!(errors_of(event(json!({"name": "x", "status": "Planned", "occurred_on": "2026-13-45"}))).contains("occurred_on"));
        let msg = errors_of(event(json!({"name": "x", "status": "Planned",
            "started_at": "2026-01-02T10:00:00Z", "ended_at": "2026-01-01T10:00:00Z"})));
        assert!(msg.contains("started_at must not be after ended_at"), "{msg}");
        // same instant in different offsets is not "after"
        assert!(event(json!({"name": "x", "status": "Planned",
            "started_at": "2026-01-01T10:00:00+01:00", "ended_at": "2026-01-01T09:00:00Z"})).is_ok());
    }

    #[test]
    fn event_vocabularies_are_enforced_and_all_errors_reported_together() {
        let msg = errors_of(event(json!({"class": "Banana", "name": "", "status": "Done", "occurred_on": "2026-01-01",
            "sensitivity": "Top", "confidence": 1.5})));
        for needle in ["class must be one of", "name must have", "status must be one of", "sensitivity must be one of", "confidence must be"] {
            assert!(msg.contains(needle), "missing '{needle}' in: {msg}");
        }
    }

    #[test]
    fn transaction_and_measurement_shapes() {
        let base = |extra: Value| {
            let mut v = json!({"class": "Transaction", "name": "Coffee", "status": "Completed", "occurred_on": "2026-01-01"});
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            v
        };
        assert!(errors_of(event(base(json!({})))).contains("payload.amount"));
        assert!(errors_of(event(base(json!({"payload": {"amount": -3.5, "currency": "euro"}})))).contains("ISO 4217"));
        assert!(event(base(json!({"payload": {"amount": -3.5, "currency": "EUR"}}))).is_ok());
        assert!(event(base(json!({"payload": {"amount": 10, "currency": "EUR", "note": "x"}}))).is_ok());

        let measurement = |payload: Value| json!({"class": "Measurement", "name": "Weight", "status": "Completed",
            "occurred_at": "2026-01-01T08:00:00Z", "payload": payload});
        assert!(event(measurement(json!({"metric": "weight", "value": 72.4, "unit": "kg"}))).is_ok());
        let msg = errors_of(event(measurement(json!({"metric": "weight"}))));
        assert!(msg.contains("payload.unit") && msg.contains("payload.value"), "{msg}");
    }

    #[test]
    fn secrets_are_refused_in_payloads_at_any_depth() {
        for payload in [json!({"password": "x"}), json!({"a": {"API_KEY": "x"}}), json!({"list": [{"Token": "x"}]}), json!({"pin": 1234})] {
            let msg = errors_of(event(json!({"name": "x", "status": "Planned", "occurred_on": "2026-01-01", "payload": payload})));
            assert!(msg.contains("secrets are never stored"), "{msg}");
        }
        assert!(event(json!({"name": "x", "status": "Planned", "occurred_on": "2026-01-01", "payload": {"tokens_used": 3, "spinning": true}})).is_ok());
    }

    #[test]
    fn payload_must_be_a_small_object() {
        assert!(errors_of(event(json!({"name": "x", "status": "Planned", "occurred_on": "2026-01-01", "payload": [1, 2]}))).contains("JSON object"));
        let big = "x".repeat(MAX_PAYLOAD_BYTES);
        assert!(errors_of(event(json!({"name": "x", "status": "Planned", "occurred_on": "2026-01-01", "payload": {"k": big}}))).contains("too large"));
        assert_eq!(event(json!({"name": "x", "status": "Planned", "occurred_on": "2026-01-01", "payload": null})).unwrap().payload, json!({}));
    }

    #[test]
    fn unknown_fields_are_rejected_by_the_input_shape() {
        let parsed: Result<EventInput, _> = serde_json::from_value(json!({"name": "x", "status": "Planned", "occurred_on": "2026-01-01", "colour": "red"}));
        assert!(parsed.is_err());
    }

    #[test]
    fn supersedes_must_be_a_uuid() {
        assert!(errors_of(event(json!({"name": "x", "status": "Planned", "occurred_on": "2026-01-01", "supersedes": "123"}))).contains("supersedes"));
        assert!(event(json!({"name": "x", "status": "Planned", "occurred_on": "2026-01-01", "supersedes": "123e4567-e89b-12d3-a456-426614174000"})).is_ok());
    }

    #[test]
    fn filter_dates() {
        assert!(is_date_or_instant("2026-05-29") && is_date_or_instant("2026-05-29T10:00:00Z") && is_date_or_instant("2026-05-29T10:00:00+01:00"));
        assert!(!is_date_or_instant("29/05/2026") && !is_date_or_instant("") && !is_date_or_instant("2026-02-30") && !is_date_or_instant("now"));
    }

    #[test]
    fn small_validators() {
        assert!(is_currency("EUR") && !is_currency("eur") && !is_currency("EURO") && !is_currency("E1R"));
        assert!(is_uuid("123e4567-e89b-12d3-a456-426614174000") && !is_uuid("123e4567e89b12d3a456426614174000") && !is_uuid("zzze4567-e89b-12d3-a456-426614174000"));
    }

    // ---- drift guards: the lists above must match the schema, the shapes and the ontology
    fn repo_file(relative: &str) -> String {
        let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), relative].iter().collect();
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
    }

    fn ts_list(source: &str, constant: &str) -> Vec<String> {
        let start = source.find(&format!("export const {constant} = [")).unwrap_or_else(|| panic!("{constant} not found in schema.ts"));
        let body = &source[start..];
        let open = body.find('[').unwrap() + 1;
        let close = body.find(']').unwrap();
        body[open..close].split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()
    }

    #[test]
    fn vocabularies_match_the_drizzle_schema() {
        let schema = repo_file("db/src/schema.ts");
        for (constant, list) in [("STATUS", &STATUSES[..]), ("SENSITIVITY", &SENSITIVITIES[..])] {
            let expected: Vec<String> = list.iter().map(|s| s.to_string()).collect();
            assert_eq!(ts_list(&schema, constant), expected, "{constant} in schema.ts differs from src/domain.rs");
        }
    }

    #[test]
    fn event_classes_match_the_ontology() {
        let ontology = repo_file("ontology/lifeui.ttl");
        // every class declared as `rdfs:subClassOf life:Event` (directly) must be in EVENT_CLASSES, and vice versa
        let mut declared: Vec<String> = ontology
            .lines()
            .filter(|l| l.contains("rdfs:subClassOf life:Event "))
            .filter_map(|l| l.trim_start().strip_prefix("life:").and_then(|r| r.split_whitespace().next()).map(str::to_string))
            .collect();
        declared.push("Event".into());
        // Award is declared as a subclass of Milestone (still an Event through it)
        assert!(ontology.contains("life:Award       a owl:Class ; rdfs:subClassOf life:Milestone"), "Award declaration changed");
        declared.push("Award".into());
        declared.sort();
        let mut expected: Vec<String> = EVENT_CLASSES.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(declared, expected, "event classes in ontology/lifeui.ttl differ from EVENT_CLASSES");
    }

    #[test]
    fn vocabulary_individuals_match_the_ontology() {
        let ontology = repo_file("ontology/lifeui.ttl");
        for value in STATUSES.iter().chain(&SENSITIVITIES) {
            assert!(ontology.contains(&format!("life:{value} ")), "life:{value} missing from the ontology");
        }
    }
}
