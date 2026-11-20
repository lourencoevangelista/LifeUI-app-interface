//! Network calls for the app screens. Everything goes through the JS glue (`public/lifeui.js`), which
//! sends the session cookie and returns a JSON string `{"status": <http status>, "data": <parsed body>}`.

use serde_json::Value;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = lifeui, catch)]
    async fn api_get(path: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = lifeui, catch)]
    async fn api_post(path: &str, body: &str) -> Result<JsValue, JsValue>;
    /// "2026-05-29" + "14:30" -> an RFC 3339 instant in UTC, or "" when the input is not a valid date and time.
    #[wasm_bindgen(js_namespace = lifeui)]
    fn local_to_instant(date: &str, time: &str) -> String;
}

pub fn to_instant(date: &str, time: &str) -> Option<String> {
    let value = local_to_instant(date, time);
    (!value.is_empty()).then_some(value)
}

/// What the screens need from a response: success with the body, or messages that are safe to show.
pub enum Outcome {
    Ok(Value),
    /// Problems to show the user (validation messages, "log in again", network errors).
    Problems(Vec<String>),
    /// 401: the session is gone, go back to the login screen.
    LoggedOut,
}

fn interpret(raw: Result<JsValue, JsValue>) -> Outcome {
    let Some(text) = raw.ok().and_then(|v| v.as_string()) else {
        return Outcome::Problems(vec!["The browser could not send the request.".into()]);
    };
    let Ok(envelope) = serde_json::from_str::<Value>(&text) else {
        return Outcome::Problems(vec!["Unreadable answer from the server.".into()]);
    };
    let status = envelope["status"].as_u64().unwrap_or(0);
    let data = envelope["data"].clone();
    match status {
        200 | 201 if data["ok"].as_bool() == Some(true) => Outcome::Ok(data),
        401 => Outcome::LoggedOut,
        _ => {
            // 400 answers carry "details": the full list of validation problems.
            let mut problems: Vec<String> = data["details"]
                .as_array()
                .map(|list| list.iter().filter_map(|e| e.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if problems.is_empty() {
                problems.push(
                    data["error"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| format!("Unexpected answer from the server (status {status}).")),
                );
            }
            Outcome::Problems(problems)
        }
    }
}

pub async fn get(path: &str) -> Outcome {
    interpret(api_get(path).await)
}

pub async fn post(path: &str, body: &Value) -> Outcome {
    interpret(api_post(path, &body.to_string()).await)
}
