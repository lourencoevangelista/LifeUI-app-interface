//! Authoritative validation of writes against the REAL SHACL shapes (ontology/lifeui.shapes.ttl).
//!
//! Layers, in the order a write passes them:
//!   1. `domain`  : parses the request, applies defaults, friendly messages (Rust mirrors of the rules);
//!   2. `shapes`  : THIS module: turns the validated item into an RDF graph, adds the ontology and runs the
//!                  shapes through the `shacl` (rudof) engine; the shapes file is the source of truth, so a rule
//!                  added there is enforced without touching Rust;
//!   3. the database CHECK constraints (db/src/schema.ts), the last line of defence.
//!
//! The graph is built from typed values (`oxrdf` nodes and literals), NEVER by assembling Turtle text, so user
//! text can only ever be a literal's content: it cannot add or change triples (tests below prove it).
//! Only violations reject a write; warnings (e.g. "no account linked yet") are ignored here.
//! The ontology and the shapes are compiled into the binary (`include_str!`), so no file access at runtime.

use std::{io::Cursor, sync::OnceLock};

use oxrdf::{
    Literal, NamedNode, Term,
    vocab::{rdf, rdfs, xsd},
};
use rudof_rdf::{
    rdf_core::RDFFormat,
    rdf_impl::{OxigraphInMemory, ReaderMode},
};
use serde_json::{Number, Value};
use sparql_service::RdfData;
use time::{Date, OffsetDateTime, format_description::well_known::{Iso8601, Rfc3339}};
use shacl::{
    ir::IRSchema,
    types::Severity,
    validator::{
        ShaclConfig, ShaclValidationMode,
        processor::{GraphValidation, ShaclProcessor},
        store::ShaclDataManager,
    },
};

use crate::{
    data::APP_SOURCE_IRI,
    domain::{ID_BASE, ONTOLOGY_BASE, ValidEvent},
};

const ONTOLOGY_TTL: &str = include_str!("../ontology/lifeui.ttl");
const SHAPES_TTL: &str = include_str!("../ontology/lifeui.shapes.ttl");

static SCHEMA: OnceLock<Result<IRSchema, String>> = OnceLock::new();

/// The compiled shapes, parsed once per function instance.
fn schema() -> Result<&'static IRSchema, String> {
    SCHEMA
        .get_or_init(|| {
            ShaclDataManager::load(&mut Cursor::new(SHAPES_TTL.as_bytes()), "lifeui.shapes.ttl", &RDFFormat::Turtle, None)
                .map_err(|e| format!("cannot load the SHACL shapes: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

// ------------------------------------------------------------------------------------ public checks

/// `Ok(problems)`: empty means the event conforms. `Err`: the validation engine itself failed (a server problem).
pub fn check_event(event: &ValidEvent) -> Result<Vec<String>, String> {
    let (triples, mut problems) = event_triples(event)?;
    problems.extend(validate(triples)?);
    Ok(problems)
}

// ------------------------------------------------------------------------------------ graph building

type Triple = (NamedNode, NamedNode, Term);

struct Graph {
    focus: NamedNode,
    triples: Vec<Triple>,
    /// Values whose text is not a well-formed date / timestamp / number. The SHACL engine only compares a literal's
    /// datatype label (it accepts "soon"^^xsd:date), so the format is checked here and the value is reported.
    problems: Vec<String>,
}

fn well_formed(value: &str, datatype: oxrdf::NamedNodeRef<'static>) -> bool {
    if datatype == xsd::DATE_TIME {
        OffsetDateTime::parse(value, &Rfc3339).is_ok()
    } else if datatype == xsd::DATE {
        Date::parse(value, &Iso8601::DATE).is_ok()
    } else if datatype == xsd::INTEGER {
        value.parse::<i128>().is_ok()
    } else if datatype == xsd::DECIMAL {
        let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
        let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        (!whole.is_empty() || !fraction.is_empty())
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.bytes().all(|b| b.is_ascii_digit())
    } else {
        true
    }
}

fn life(local: &str) -> Result<NamedNode, String> {
    NamedNode::new(format!("{ONTOLOGY_BASE}{local}")).map_err(|e| format!("'{local}' cannot be part of an IRI: {e}"))
}

impl Graph {
    fn new(focus_iri: String) -> Result<Self, String> {
        let focus = NamedNode::new(focus_iri).map_err(|e| e.to_string())?;
        Ok(Self { focus, triples: Vec::new(), problems: Vec::new() })
    }

    fn add(&mut self, predicate: NamedNode, object: Term) {
        self.triples.push((self.focus.clone(), predicate, object));
    }

    fn class(&mut self, local: &str) -> Result<(), String> {
        self.add(rdf::TYPE.into(), Term::NamedNode(life(local)?));
        Ok(())
    }

    /// `life:<predicate> life:<individual>` (vocabulary values such as life:Completed).
    fn individual(&mut self, predicate: &str, local: &str) -> Result<(), String> {
        self.add(life(predicate)?, Term::NamedNode(life(local)?));
        Ok(())
    }

    fn text(&mut self, predicate: &str, value: &str) -> Result<(), String> {
        self.add(life(predicate)?, Term::Literal(Literal::new_simple_literal(value)));
        Ok(())
    }

    fn typed(&mut self, predicate: &str, value: &str, datatype: oxrdf::NamedNodeRef<'static>) -> Result<(), String> {
        if !well_formed(value, datatype) {
            let kind = datatype.as_str().rsplit('#').next().unwrap_or("typed");
            self.problems.push(format!("Shape rule: {predicate} is not a valid {kind} value."));
            return Ok(());
        }
        self.add(life(predicate)?, Term::Literal(Literal::new_typed_literal(value, datatype)));
        Ok(())
    }

    fn number(&mut self, predicate: &str, number: &Number) -> Result<(), String> {
        let (lexical, datatype) = number_lexical(number);
        self.typed(predicate, &lexical, datatype)
    }

    /// Provenance every app-entered fact carries (shapes: LifeEntityShape, SourceShape).
    fn provenance(&mut self, sensitivity: &str, confidence: f64) -> Result<(), String> {
        self.individual("hasSensitivity", sensitivity)?;
        self.typed("confidence", &format!("{confidence:.4}"), xsd::DECIMAL)?;
        let source = NamedNode::new(APP_SOURCE_IRI).map_err(|e| e.to_string())?;
        self.add(life("hasSource")?, Term::NamedNode(source.clone()));
        self.triples.push((source.clone(), rdf::TYPE.into(), Term::NamedNode(life("Source")?)));
        self.triples.push((source.clone(), rdfs::LABEL.into(), Term::Literal(Literal::new_simple_literal("Entered in the LifeUI app"))));
        self.triples.push((source, life("hasSourceKind")?, Term::NamedNode(life("UserStated")?)));
        Ok(())
    }
}

/// JSON number -> RDF literal: whole numbers are xsd:integer, others xsd:decimal (the shapes accept
/// decimal or integer for amounts and values).
fn number_lexical(number: &Number) -> (String, oxrdf::NamedNodeRef<'static>) {
    if number.is_i64() || number.is_u64() {
        return (number.to_string(), xsd::INTEGER);
    }
    let mut text = format!("{:.12}", number.as_f64().unwrap_or(0.0));
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.push('0');
    }
    (text, xsd::DECIMAL)
}

fn event_triples(event: &ValidEvent) -> Result<(Vec<Triple>, Vec<String>), String> {
    let class = event.class_iri.strip_prefix(ONTOLOGY_BASE).ok_or("event class is not in the LifeUI ontology namespace")?;
    let mut graph = Graph::new(format!("{ID_BASE}event-pending"))?;
    graph.class(class)?;
    graph.text("name", &event.name)?;
    if let Some(description) = &event.description {
        graph.text("description", description)?;
    }
    graph.individual("hasStatus", &event.status)?;
    for (predicate, value) in [("occurredAt", &event.occurred_at), ("startedAt", &event.started_at), ("endedAt", &event.ended_at)] {
        if let Some(value) = value {
            graph.typed(predicate, value, xsd::DATE_TIME)?;
        }
    }
    if let Some(value) = &event.occurred_on {
        graph.typed("occurredOn", value, xsd::DATE)?;
    }
    graph.provenance(&event.sensitivity, event.confidence)?;

    // class-specific properties live in the payload (shapes: TransactionShape, MeasurementShape)
    match class {
        "Transaction" => {
            if let Some(Value::Number(amount)) = event.payload.get("amount") {
                graph.number("amount", amount)?;
            }
            if let Some(Value::String(currency)) = event.payload.get("currency") {
                graph.text("currency", currency)?;
            }
        }
        "Measurement" => {
            for key in ["metric", "unit"] {
                if let Some(Value::String(text)) = event.payload.get(key) {
                    graph.text(key, text)?;
                }
            }
            if let Some(Value::Number(value)) = event.payload.get("value") {
                graph.number("value", value)?;
            }
        }
        _ => {}
    }
    Ok((graph.triples, graph.problems))
}

// ------------------------------------------------------------------------------------ validation

fn validate(triples: Vec<Triple>) -> Result<Vec<String>, String> {
    let schema = schema()?;
    let mut store = OxigraphInMemory::new();
    // The ontology supplies the class hierarchy (Milestone is an Event ...) and the vocabulary individuals
    // (life:Completed a life:Status ...) that `sh:targetClass` and `sh:class` need.
    store
        .merge_from_reader(&mut Cursor::new(ONTOLOGY_TTL.as_bytes()), "lifeui.ttl", &RDFFormat::Turtle, None, &ReaderMode::default())
        .map_err(|e| format!("cannot load the ontology: {e}"))?;
    for (subject, predicate, object) in &triples {
        store.add_triple_ref(subject, predicate, object).map_err(|e| e.to_string())?;
    }
    let data = RdfData::from_graph(store).map_err(|e| format!("cannot wrap the graph for validation: {e}"))?;
    let mut validator = GraphValidation::new(data.into());
    let report = validator
        .validate(schema, &ShaclValidationMode::Native, &ShaclConfig::default())
        .map_err(|e| format!("SHACL validation failed to run: {e}"))?;

    Ok(report
        .results()
        .iter()
        .filter(|result| matches!(result.severity(), Severity::Violation))
        .map(|result| {
            let component = result.constraint_component().to_string();
            let component = component.rsplit(['#', '/']).next().unwrap_or(&component).trim_end_matches('>').to_string();
            let message = result.message().messages().values().find(|m| !m.trim().is_empty()).cloned();
            let path = result.path().map(|p| p.to_string());
            match (message, path) {
                (Some(message), _) => format!("Shape rule: {message}"),
                (None, Some(path)) => format!("Shape rule {component} failed at {path}."),
                (None, None) => format!("Shape rule {component} failed."),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EventInput, validate_event};
    use serde_json::json;

    fn event(value: Value) -> ValidEvent {
        validate_event(serde_json::from_value::<EventInput>(value).unwrap()).expect("valid by the Rust rules")
    }
    fn base_event() -> ValidEvent {
        event(json!({"class": "Milestone", "name": "Kick-off", "status": "Completed", "occurred_at": "2026-05-29T10:00:00Z"}))
    }
    fn problems(event: &ValidEvent) -> String {
        check_event(event).expect("engine runs").join(" | ")
    }

    #[test]
    fn everything_the_rust_rules_accept_conforms_to_the_shapes() {
        let inputs = [
            json!({"name": "Plain", "status": "Planned", "occurred_on": "2026-11-08"}),
            json!({"class": "Meeting", "name": "Sync", "status": "Planned", "started_at": "2026-06-01T09:00:00Z", "ended_at": "2026-06-01T09:30:00Z"}),
            json!({"class": "Award", "name": "Merit", "status": "Completed", "occurred_on": "2023-07-01", "description": "x", "confidence": 0.8, "sensitivity": "Public"}),
            json!({"class": "Workout", "name": "Run", "status": "Completed", "occurred_at": "2026-01-15T07:00:00+01:00"}),
            json!({"class": "Transaction", "name": "Coffee", "status": "Completed", "occurred_on": "2026-05-30", "payload": {"amount": -3.5, "currency": "EUR"}}),
            json!({"class": "Transaction", "name": "Salary", "status": "Completed", "occurred_on": "2026-05-30", "payload": {"amount": 1500, "currency": "EUR", "note": "x"}}),
            json!({"class": "Measurement", "name": "Weight", "status": "Completed", "occurred_at": "2026-01-01T08:00:00Z", "payload": {"metric": "weight", "value": 72.4, "unit": "kg"}}),
            json!({"class": "Measurement", "name": "Steps", "status": "Completed", "occurred_at": "2026-01-01T08:00:00Z", "payload": {"metric": "steps", "value": 9000, "unit": "1"}}),
        ];
        for input in inputs {
            let e = event(input.clone());
            assert_eq!(check_event(&e).unwrap(), Vec::<String>::new(), "rejected by the shapes: {input}");
        }
    }

    #[test]
    fn shapes_reject_bad_events_even_when_the_rust_rules_are_bypassed() {
        let cases: Vec<(&str, Box<dyn Fn(&mut ValidEvent)>)> = vec![
            ("confidence above 1", Box::new(|e| e.confidence = 1.5)),
            ("status outside the vocabulary", Box::new(|e| e.status = "Done".into())),
            ("sensitivity outside the vocabulary", Box::new(|e| e.sensitivity = "Top".into())),
            ("no time at all", Box::new(|e| e.occurred_at = None)),
            ("empty name", Box::new(|e| e.name = String::new())),
            ("started after ended", Box::new(|e| {
                e.occurred_at = None;
                e.started_at = Some("2026-01-02T10:00:00Z".into());
                e.ended_at = Some("2026-01-01T10:00:00Z".into());
            })),
        ];
        for (label, mutate) in cases {
            let mut e = base_event();
            mutate(&mut e);
            let found = problems(&e);
            assert!(!found.is_empty(), "the shapes accepted: {label}");
        }
        assert!(problems(&base_event()).is_empty(), "control: the untouched event conforms");
    }

    #[test]
    fn shapes_reject_bad_transactions_and_measurements() {
        let mut t = event(json!({"class": "Transaction", "name": "Coffee", "status": "Completed", "occurred_on": "2026-05-30", "payload": {"amount": -3.5, "currency": "EUR"}}));
        t.payload = json!({"amount": -3.5, "currency": "euro"});
        assert!(!problems(&t).is_empty(), "currency must be an ISO 4217 code");
        t.payload = json!({"currency": "EUR"});
        assert!(!problems(&t).is_empty(), "amount is required");
        t.payload = json!({"amount": -3.5});
        assert!(!problems(&t).is_empty(), "currency is required");

        let mut m = event(json!({"class": "Measurement", "name": "Weight", "status": "Completed", "occurred_at": "2026-01-01T08:00:00Z", "payload": {"metric": "weight", "value": 72.4, "unit": "kg"}}));
        m.payload = json!({"metric": "weight", "value": 72.4});
        assert!(!problems(&m).is_empty(), "unit is required");
        m.payload = json!({"value": 72.4, "unit": "kg"});
        assert!(!problems(&m).is_empty(), "metric is required");
    }

    #[test]
    fn a_missing_account_is_only_a_warning_for_now() {
        // TransactionShape asks for life:inAccount, but there is no Account table yet: it is a Warning, not a Violation.
        let t = event(json!({"class": "Transaction", "name": "Coffee", "status": "Completed", "occurred_on": "2026-05-30", "payload": {"amount": -3.5, "currency": "EUR"}}));
        assert!(check_event(&t).unwrap().is_empty());
    }

    #[test]
    fn hostile_text_is_only_ever_text() {
        // These would break out of a Turtle string or add facts if the graph were built by pasting text together.
        let hostile = "x\"\"\" ; life:hasStatus life:Cancelled ; life:name \"pwned\n<http://evil/>\\u0022 .\r\n# comment";
        let mut e = base_event();
        e.name = hostile.to_string();
        e.description = Some(hostile.to_string());
        assert!(problems(&e).is_empty(), "hostile characters must not break validation: {}", problems(&e));

        let (triples, malformed) = event_triples(&e).unwrap();
        assert!(malformed.is_empty());
        let names: Vec<&Term> = triples.iter().filter(|(_, p, _)| p.as_str().ends_with("#name")).map(|(_, _, o)| o).collect();
        assert_eq!(names.len(), 1, "exactly one name triple, nothing injected");
        match names[0] {
            Term::Literal(l) => assert_eq!(l.value(), hostile, "the text is stored verbatim as the literal"),
            other => panic!("name is not a literal: {other:?}"),
        }
        let statuses = triples.iter().filter(|(_, p, _)| p.as_str().ends_with("#hasStatus")).count();
        assert_eq!(statuses, 1, "no extra status was injected");
    }

    #[test]
    fn number_formatting() {
        let n = |v: Value| match v { Value::Number(n) => n, _ => unreachable!() };
        assert_eq!(number_lexical(&n(json!(10))), ("10".to_string(), xsd::INTEGER));
        assert_eq!(number_lexical(&n(json!(-3.5))), ("-3.5".to_string(), xsd::DECIMAL));
        assert_eq!(number_lexical(&n(json!(10.0))), ("10.0".to_string(), xsd::DECIMAL));
        assert_eq!(number_lexical(&n(json!(0.000001))), ("0.000001".to_string(), xsd::DECIMAL));
    }

    #[test]
    fn one_validation_is_fast_enough_for_a_request() {
        let start = std::time::Instant::now();
        assert!(check_event(&base_event()).unwrap().is_empty());
        let first = start.elapsed();
        let start = std::time::Instant::now();
        assert!(check_event(&base_event()).unwrap().is_empty());
        let second = start.elapsed();
        assert!(second < std::time::Duration::from_secs(2), "too slow: first {first:?}, second {second:?}");
    }
}
