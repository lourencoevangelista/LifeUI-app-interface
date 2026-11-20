//! The signed-in app: timeline of events, the daily news briefing, the read-only knowledge base, and the forms
//! that add to them (shown in a bottom sheet).
//!
//! Nothing is edited or deleted in place: "New version" pre-fills the form and the server stores a NEW row
//! that supersedes the old one (the old version stays in the database). The lists below the server's
//! validation are only convenience: the API validates everything again (domain.rs, SHACL, CHECK constraints).
//!
//! The class names used here (tabs, tab, card, row, pick, detail, links, problems, warn ...) are styled in
//! web/style.css and looked up by tools/ui_test.py: change them only together with those two files.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use crate::api::{self, Outcome};

#[wasm_bindgen]
extern "C" {
    /// Today's date in the browser's local time, "YYYY-MM-DD".
    #[wasm_bindgen(js_namespace = lifeui)]
    fn today() -> String;
    /// Opens or closes the bottom sheet (the native <dialog id="sheet">); see public/lifeui.js.
    #[wasm_bindgen(js_namespace = lifeui)]
    fn sheet(open: bool);
    /// Shows `value` in the span with this id with an animated-digits counter (NumberFlow).
    #[wasm_bindgen(js_namespace = lifeui)]
    fn flow(id: &str, value: f64);
}

// Mirrors of the Rust lists in src/domain.rs (the server rejects anything else anyway).
const EVENT_CLASSES: [&str; 9] = ["Event", "Appointment", "Meeting", "Milestone", "Trip", "Workout", "Measurement", "Transaction", "Award"];
const STATUSES: [&str; 5] = ["Planned", "InProgress", "Completed", "Cancelled", "Blocked"];
const SENSITIVITIES: [&str; 4] = ["Public", "Personal", "Sensitive", "Secret"];

pub(crate) const LOADING: &str = "Loading...";

pub(crate) fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

/// "2026-05-29T10:00:00+00:00" -> "2026-05-29 10:00"; a plain date stays a date.
pub(crate) fn short_time(raw: &str) -> String {
    raw.chars().take(16).collect::<String>().replace('T', " ")
}

fn when_label(event: &Value) -> String {
    for key in ["occurred_at", "started_at", "occurred_on"] {
        if let Some(raw) = event[key].as_str() {
            return short_time(raw);
        }
    }
    String::from("no date")
}

/// "InProgress" -> "In progress"
fn humanize(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ------------------------------------------------------------------------------------ small building blocks

/// Line icons (24x24, 2 px stroke, round caps) in the style of Lucide (ISC licence); the shapes are drawn here.
#[component]
pub fn Icon(name: &'static str, #[prop(optional)] class: &'static str) -> impl IntoView {
    let shapes = match name {
        "home" => r#"<path d="M3 11.5 12 4l9 7.5"/><path d="M5.5 10v9.5a1 1 0 0 0 1 1H10v-5h4v5h3.5a1 1 0 0 0 1-1V10"/>"#,
        "agent" => r#"<path d="M12 3.5 13.9 9l5.6 1.9-5.6 1.9L12 18.3l-1.9-5.5L4.5 10.9 10.1 9z"/><path d="M19 3v4M17 5h4"/>"#,
        "timeline" => r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>"#,
        "explore" => r#"<circle cx="12" cy="12" r="9"/><path d="m15.5 8.5-2 5-5 2 2-5z"/>"#,
        "news" => r#"<path d="M5 4.5h11a1.5 1.5 0 0 1 1.5 1.5v13a1.5 1.5 0 0 0 1.5 1.5H6.5A2.5 2.5 0 0 1 4 18V5.5a1 1 0 0 1 1-1z"/><path d="M17.5 9.5H20v9.5a1.5 1.5 0 0 1-3 0M8 8.5h6M8 12h6M8 15.5h3.5"/>"#,
        "sun" => r#"<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M4.6 4.6l1.4 1.4M18 18l1.4 1.4M2.5 12h2M19.5 12h2M4.6 19.4 6 18M18 6l1.4-1.4"/>"#,
        "tools" => r#"<path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2"/><circle cx="9" cy="17" r="2"/>"#,
        "plus" => r#"<path d="M12 5v14M5 12h14"/>"#,
        "logout" => r#"<path d="M9 21H5.5A1.5 1.5 0 0 1 4 19.5v-15A1.5 1.5 0 0 1 5.5 3H9"/><path d="m16 17 5-5-5-5M21 12H9"/>"#,
        "chevron" => r#"<path d="m6 9 6 6 6-6"/>"#,
        "back" => r#"<path d="M19 12H5M12 19l-7-7 7-7"/>"#,
        "inbox" => r#"<path d="M22 12h-6l-2 3h-4l-2-3H2"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/>"#,
        _ => "",
    };
    view! {
        <svg class=format!("icon {class}") viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" inner_html=shapes></svg>
    }
}

/// A number whose digits animate when it changes (NumberFlow, vendored: see web/vendor/README.md).
#[component]
fn Flow(id: &'static str, #[prop(into)] value: Signal<usize>) -> impl IntoView {
    Effect::new(move |_| flow(id, value.get() as f64));
    view! { <span id=id class="flow">{value.get_untracked()}</span> }
}

#[component]
fn Options(items: &'static [&'static str]) -> impl IntoView {
    items.iter().map(|item| view! { <option value=*item>{*item}</option> }).collect_view()
}

#[component]
pub(crate) fn Problems(messages: ReadSignal<Vec<String>>) -> impl IntoView {
    view! {
        <ul class="problems" role="alert">
            {move || messages.get().into_iter().map(|m| view! { <li>{m}</li> }).collect_view()}
        </ul>
    }
}

/// What a list screen shows besides its items: placeholder cards while loading, otherwise the status message.
#[component]
pub(crate) fn ListStatus(status: ReadSignal<String>) -> impl IntoView {
    view! {
        <Show when=move || status.get() == LOADING>
            <div class="skeletons" aria-hidden="true"><div class="skeleton"></div><div class="skeleton"></div><div class="skeleton"></div></div>
        </Show>
        <Show when=move || status.get() != LOADING && !status.get().is_empty()>
            <div class="empty"><Icon name="inbox" /><p>{move || status.get()}</p></div>
        </Show>
    }
}

/// A "Show more / less" row that works with the keyboard as well as the pointer.
fn row_keys(toggle: impl Fn() + Clone + 'static) -> impl Fn(leptos::ev::KeyboardEvent) + 'static {
    move |ev| {
        if ev.key() == "Enter" || ev.key() == " " {
            ev.prevent_default();
            toggle();
        }
    }
}

// ------------------------------------------------------------------------------------ bottom sheet state

#[derive(Clone, Copy, PartialEq)]
enum Sheet {
    Closed,
    Event,
}

/// The bottom sheet that holds the add / new-version forms. `generation` makes every opening build a fresh form.
#[derive(Clone, Copy)]
struct SheetCtl {
    kind: RwSignal<Sheet>,
    open: RwSignal<bool>,
    generation: RwSignal<u32>,
}

impl SheetCtl {
    fn new() -> Self {
        Self { kind: RwSignal::new(Sheet::Closed), open: RwSignal::new(false), generation: RwSignal::new(0) }
    }

    fn show(self, kind: Sheet) {
        self.kind.set(kind);
        self.generation.update(|g| *g += 1);
        self.open.set(true);
    }

    /// Starts the closing animation, then drops the form once the sheet is gone.
    fn hide(self) {
        self.open.set(false);
        set_timeout(
            move || {
                if !self.open.get_untracked() {
                    self.kind.set(Sheet::Closed);
                }
            },
            Duration::from_millis(320),
        );
    }
}

// ------------------------------------------------------------------------------------ timeline

#[component]
fn Timeline(
    set_screen: WriteSignal<String>,
    refresh: RwSignal<u32>,
    edit_event: RwSignal<Option<Value>>,
    sheet: SheetCtl,
) -> impl IntoView {
    let (events, set_events) = signal(Vec::<Value>::new());
    let (status, set_status) = signal(String::from(LOADING));
    let (class, set_class) = signal(String::new());
    let (open, set_open) = signal(String::new());

    Effect::new(move |_| {
        refresh.track();
        let filter = class.get();
        set_status.set(LOADING.into());
        spawn_local(async move {
            let path = if filter.is_empty() { "/api/events?limit=100".to_string() } else { format!("/api/events?limit=100&class={filter}") };
            match api::get(&path).await {
                Outcome::Ok(data) => {
                    let list = data["events"].as_array().cloned().unwrap_or_default();
                    set_status.set(if list.is_empty() { "Nothing here yet. Add the first event.".into() } else { String::new() });
                    set_events.set(list);
                }
                Outcome::Problems(p) => set_status.set(p.join(" ")),
                Outcome::LoggedOut => set_screen.set("login".into()),
            }
        });
    });

    view! {
        <section>
            <div class="screen-head">
                <h2>"Timeline"</h2>
                <span class="count"><Flow id="count-timeline" value=Signal::derive(move || events.get().len()) />" events"</span>
            </div>
            <label>
                "Show"
                <select on:change=move |ev| set_class.set(event_target_value(&ev))>
                    <option value="">"All kinds"</option>
                    <Options items=&EVENT_CLASSES />
                </select>
            </label>
            <ListStatus status />
            <For each=move || events.get() key=|e| text(e, "id") children=move |event: Value| {
                let id = text(&event, "id");
                let is_open = {
                    let id = id.clone();
                    move || open.get() == id
                };
                let toggle = {
                    let id = id.clone();
                    move || set_open.update(|o| *o = if *o == id { String::new() } else { id.clone() })
                };
                let details = event.clone();
                let for_edit = event.clone();
                let state = text(&event, "status");
                view! {
                    <article class="card">
                        <div class="row" class:open=is_open.clone() role="button" tabindex="0"
                            aria-expanded={ let is_open = is_open.clone(); move || is_open().to_string() }
                            on:click={ let toggle = toggle.clone(); move |_| toggle() }
                            on:keydown=row_keys(toggle.clone())>
                            <span class="when">{when_label(&event)}</span>
                            <span class="kind">{text(&event, "class")}</span>
                            <span class=format!("badge s-{}", text(&event, "sensitivity"))>{text(&event, "sensitivity")}</span>
                            <Icon name="chevron" class="chevron" />
                        </div>
                        <strong>{text(&event, "name")}</strong>
                        <span class=format!("status st-{state}")>{humanize(&state)}</span>
                        <Show when=is_open>
                            <div class="more">
                                <p>{text(&details, "description")}</p>
                                <button class="btn secondary sm" on:click={
                                    let for_edit = for_edit.clone();
                                    move |_| {
                                        edit_event.set(Some(for_edit.clone()));
                                        sheet.show(Sheet::Event);
                                    }
                                }>"New version"</button>
                            </div>
                        </Show>
                    </article>
                }
            } />
        </section>
    }
}

// ------------------------------------------------------------------------------------ event form

#[component]
fn EventForm(
    set_screen: WriteSignal<String>,
    refresh: RwSignal<u32>,
    edit_event: RwSignal<Option<Value>>,
    sheet: SheetCtl,
) -> impl IntoView {
    let old = edit_event.get_untracked();
    let get = |k: &str| old.as_ref().map(|e| text(e, k)).unwrap_or_default();
    let payload = |k: &str| {
        old.as_ref()
            .map(|e| match &e["payload"][k] {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => String::new(),
            })
            .unwrap_or_default()
    };
    // an Agent proposal opened with Edit has no id (it is not an event yet) but names its inbox item
    let supersedes = old.as_ref().map(|e| text(e, "id")).filter(|id| !id.is_empty());
    let from_proposal = old.as_ref().map(|e| text(e, "_from_proposal")).filter(|id| !id.is_empty());
    let proposed = from_proposal.is_some();
    let initial_date: String = {
        let raw = old.as_ref().and_then(|e| ["occurred_on", "occurred_at", "started_at"].iter().find_map(|k| e[*k].as_str().map(String::from)));
        raw.map(|r| r.chars().take(10).collect()).unwrap_or_else(today)
    };

    let class = RwSignal::new(if get("class").is_empty() { "Event".to_string() } else { get("class") });
    let name = RwSignal::new(get("name"));
    let status = RwSignal::new(if get("status").is_empty() { "Completed".to_string() } else { get("status") });
    let date = RwSignal::new(initial_date);
    let time = RwSignal::new(get("_time"));
    let sensitivity = RwSignal::new(if get("sensitivity").is_empty() { "Personal".to_string() } else { get("sensitivity") });
    let description = RwSignal::new(get("description"));
    let amount = RwSignal::new(payload("amount"));
    let currency = RwSignal::new(if payload("currency").is_empty() { "EUR".to_string() } else { payload("currency") });
    let metric = RwSignal::new(payload("metric"));
    let unit = RwSignal::new(payload("unit"));
    let value = RwSignal::new(payload("value"));
    let (problems, set_problems) = signal(Vec::<String>::new());
    let (busy, set_busy) = signal(false);
    let editing = supersedes.is_some();

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let mut body = json!({
            "class": class.get(), "name": name.get(), "status": status.get(), "sensitivity": sensitivity.get(),
        });
        let note = description.get();
        if !note.trim().is_empty() {
            body["description"] = json!(note);
        }
        if time.get().is_empty() {
            body["occurred_on"] = json!(date.get());
        } else if let Some(instant) = api::to_instant(&date.get(), &time.get()) {
            body["occurred_at"] = json!(instant);
        } else {
            set_problems.set(vec!["That date and time is not valid.".into()]);
            return;
        }
        let number = |s: String| s.trim().replace(',', ".").parse::<f64>().ok();
        match class.get().as_str() {
            "Transaction" => {
                body["payload"] = json!({ "amount": number(amount.get()), "currency": currency.get().trim().to_uppercase() });
            }
            "Measurement" => {
                body["payload"] = json!({ "metric": metric.get(), "unit": unit.get(), "value": number(value.get()) });
            }
            _ => {}
        }
        if let Some(id) = &supersedes {
            body["supersedes"] = json!(id);
        }
        if let Some(id) = &from_proposal {
            body["from_proposal"] = json!(id);
        }
        set_busy.set(true);
        set_problems.set(vec![]);
        spawn_local(async move {
            match api::post("/api/events", &body).await {
                Outcome::Ok(_) => {
                    edit_event.set(None);
                    refresh.update(|n| *n += 1);
                    sheet.hide();
                }
                Outcome::Problems(p) => set_problems.set(p),
                Outcome::LoggedOut => set_screen.set("login".into()),
            }
            set_busy.set(false);
        });
    };

    view! {
        <form on:submit=submit>
            <h2>{if editing { "New version of an event" } else if proposed { "Add the proposed event" } else { "Add an event" }}</h2>
            <Show when=move || proposed>
                <p class="muted">"Prepared by the agent from your inbox. Check it, change what is wrong, then save."</p>
            </Show>
            <Show when=move || editing>
                <p class="muted">"The old version stays in the database. Re-enter the time if it had one."</p>
            </Show>
            <label>"Kind"
                <select prop:value=move || class.get() on:change=move |ev| class.set(event_target_value(&ev))>
                    <Options items=&EVENT_CLASSES />
                </select>
            </label>
            <label>"What happened"
                <input type="text" maxlength="200" required prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev)) />
            </label>
            <label>"Date"
                <input type="date" required prop:value=move || date.get() on:input=move |ev| date.set(event_target_value(&ev)) />
            </label>
            <label>"Time (optional)"
                <input type="time" prop:value=move || time.get() on:input=move |ev| time.set(event_target_value(&ev)) />
            </label>
            <label>"Status"
                <select prop:value=move || status.get() on:change=move |ev| status.set(event_target_value(&ev))>
                    <Options items=&STATUSES />
                </select>
            </label>
            <Show when=move || class.get() == "Transaction">
                <label>"Amount (negative = money out)"
                    <input type="text" inputmode="decimal" prop:value=move || amount.get() on:input=move |ev| amount.set(event_target_value(&ev)) />
                </label>
                <label>"Currency (3 letters)"
                    <input type="text" maxlength="3" prop:value=move || currency.get() on:input=move |ev| currency.set(event_target_value(&ev)) />
                </label>
            </Show>
            <Show when=move || class.get() == "Measurement">
                <label>"Metric (e.g. weight)"
                    <input type="text" prop:value=move || metric.get() on:input=move |ev| metric.set(event_target_value(&ev)) />
                </label>
                <label>"Value"
                    <input type="text" inputmode="decimal" prop:value=move || value.get() on:input=move |ev| value.set(event_target_value(&ev)) />
                </label>
                <label>"Unit (e.g. kg)"
                    <input type="text" prop:value=move || unit.get() on:input=move |ev| unit.set(event_target_value(&ev)) />
                </label>
            </Show>
            <label>"Sensitivity"
                <select prop:value=move || sensitivity.get() on:change=move |ev| sensitivity.set(event_target_value(&ev))>
                    <Options items=&SENSITIVITIES />
                </select>
            </label>
            <label>"Notes (optional)"
                <textarea rows="3" maxlength="2000" prop:value=move || description.get() on:input=move |ev| description.set(event_target_value(&ev))></textarea>
            </label>
            <Problems messages=problems />
            <div class="actions">
                <button type="button" class="btn secondary" on:click=move |_| { edit_event.set(None); sheet.hide(); }>"Cancel"</button>
                <button type="submit" class="btn" prop:disabled=move || busy.get()>{move || if busy.get() { "Saving..." } else { "Save" }}</button>
            </div>
        </form>
    }
}

// ------------------------------------------------------------------------------------ explore (read-only knowledge base)

/// A property value for display: text as is, numbers, and lists joined with commas.
fn show_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(show_value).collect::<Vec<_>>().join(", "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// "ownedBy" -> "owned by"
fn predicate_label(predicate: &str) -> String {
    let mut out = String::new();
    for ch in predicate.chars() {
        if ch.is_uppercase() {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[component]
fn Relations(title: &'static str, links: Vec<Value>, open: WriteSignal<String>) -> impl IntoView {
    if links.is_empty() {
        return ().into_any();
    }
    view! {
        <h3>{title}</h3>
        <ul class="links">
            {links.into_iter().map(|link| {
                let iri = text(&link, "iri");
                let name = if text(&link, "name").is_empty() { iri.clone() } else { text(&link, "name") };
                // only entities open here; events and sources live elsewhere
                let target = if text(&link, "kind") == "entity" {
                    view! { <a href="#" on:click=move |ev| { ev.prevent_default(); open.set(iri.clone()); }>{name}</a> }.into_any()
                } else {
                    view! { <span>{name}</span> }.into_any()
                };
                view! {
                    <li>
                        <span class="muted">{predicate_label(&text(&link, "predicate"))}</span>
                        " "
                        {target}
                        " "
                        <span class="kind">{text(&link, "class")}</span>
                    </li>
                }
            }).collect_view()}
        </ul>
    }
    .into_any()
}

#[component]
fn EntityDetail(set_screen: WriteSignal<String>, iri: ReadSignal<String>, open: WriteSignal<String>) -> impl IntoView {
    let (entity, set_entity) = signal(None::<Value>);
    let (out, set_out) = signal(Vec::<Value>::new());
    let (incoming, set_incoming) = signal(Vec::<Value>::new());
    let (status, set_status) = signal(String::from(LOADING));

    Effect::new(move |_| {
        let target = iri.get();
        set_status.set(LOADING.into());
        set_entity.set(None);
        spawn_local(async move {
            let path = format!("/api/entities?iri={}", js_sys_encode(&target));
            match api::get(&path).await {
                Outcome::Ok(data) => {
                    set_out.set(data["links_out"].as_array().cloned().unwrap_or_default());
                    set_incoming.set(data["links_in"].as_array().cloned().unwrap_or_default());
                    set_entity.set(Some(data["entity"].clone()));
                    set_status.set(String::new());
                }
                Outcome::Problems(p) => set_status.set(p.join(" ")),
                Outcome::LoggedOut => set_screen.set("login".into()),
            }
        });
    });

    view! {
        <ListStatus status />
        {move || entity.get().map(|e| {
            let props: Vec<(String, String)> = e["props"].as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), show_value(v))).collect())
                .unwrap_or_default();
            let state = text(&e, "status");
            view! {
                <article class="card detail">
                    <div class="row">
                        <span class="kind">{text(&e, "class")}</span>
                        <span class=format!("badge s-{}", text(&e, "sensitivity"))>{text(&e, "sensitivity")}</span>
                        {(!state.is_empty()).then(|| view! { <span class=format!("status st-{state}")>{humanize(&state)}</span> })}
                    </div>
                    <strong>{text(&e, "name")}</strong>
                    <span class="muted">{format!("confidence {} - source: {}", show_value(&e["confidence"]), text(&e, "source"))}</span>
                    <dl>
                        {props.into_iter().map(|(k, v)| view! { <dt>{predicate_label(&k)}</dt><dd>{v}</dd> }).collect_view()}
                    </dl>
                </article>
            }
        })}
        {move || view! { <Relations title="Links from this" links=out.get() open /> }}
        {move || view! { <Relations title="Links to this" links=incoming.get() open /> }}
    }
}

/// Minimal percent-encoding for the iri query value (the browser would do it, but fetch gets the string as is).
fn js_sys_encode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[component]
fn Explore(set_screen: WriteSignal<String>) -> impl IntoView {
    let (items, set_items) = signal(Vec::<Value>::new());
    let (classes, set_classes) = signal(Vec::<Value>::new());
    let (status, set_status) = signal(String::from(LOADING));
    let (class, set_class) = signal(String::new());
    let (query, set_query) = signal(String::new());
    let (shown_iri, set_shown_iri) = signal(String::new());

    Effect::new(move |_| {
        let (c, q) = (class.get(), query.get());
        spawn_local(async move {
            let mut path = String::from("/api/entities?limit=100");
            if !c.is_empty() {
                path.push_str(&format!("&class={c}"));
            }
            if !q.trim().is_empty() {
                path.push_str(&format!("&q={}", js_sys_encode(q.trim())));
            }
            match api::get(&path).await {
                Outcome::Ok(data) => {
                    let list = data["entities"].as_array().cloned().unwrap_or_default();
                    set_status.set(if list.is_empty() { "Nothing matches.".into() } else { String::new() });
                    set_items.set(list);
                    set_classes.set(data["classes"].as_array().cloned().unwrap_or_default());
                }
                Outcome::Problems(p) => set_status.set(p.join(" ")),
                Outcome::LoggedOut => set_screen.set("login".into()),
            }
        });
    });

    view! {
        <Show when=move || !shown_iri.get().is_empty() fallback=move || view! {
            <section>
                <div class="screen-head">
                    <h2>"Explore"</h2>
                    <span class="count"><Flow id="count-explore" value=Signal::derive(move || items.get().len()) />" shown"</span>
                </div>
                <p class="muted">"Documents, projects, tasks, courses, people and tools from your knowledge base."</p>
                <label>"Kind"
                    <select on:change=move |ev| set_class.set(event_target_value(&ev))>
                        <option value="">"All kinds"</option>
                        {move || classes.get().into_iter().map(|c| {
                            let name = text(&c, "class");
                            view! { <option value=name.clone()>{format!("{} ({})", name, show_value(&c["count"]))}</option> }
                        }).collect_view()}
                    </select>
                </label>
                <label>"Search by name"
                    <input type="search" maxlength="100" placeholder="Type to filter..." on:input=move |ev| set_query.set(event_target_value(&ev)) />
                </label>
                <ListStatus status />
                <For each=move || items.get() key=|e| text(e, "iri") children=move |e: Value| {
                    let iri = text(&e, "iri");
                    let open_it = {
                        let iri = iri.clone();
                        move || set_shown_iri.set(iri.clone())
                    };
                    let state = text(&e, "status");
                    view! {
                        <article class="card pick" role="button" tabindex="0" on:click={ let open_it = open_it.clone(); move |_| open_it() }
                            on:keydown=row_keys(open_it.clone())>
                            <div class="row">
                                <span class="kind">{text(&e, "class")}</span>
                                <span class=format!("badge s-{}", text(&e, "sensitivity"))>{text(&e, "sensitivity")}</span>
                                {(!state.is_empty()).then(|| view! { <span class=format!("status st-{state}")>{humanize(&state)}</span> })}
                            </div>
                            <strong>{text(&e, "name")}</strong>
                        </article>
                    }
                } />
            </section>
        }>
            <section>
                <button class="btn ghost sm" on:click=move |_| set_shown_iri.set(String::new())><Icon name="back" />"Back to the list"</button>
                <EntityDetail set_screen iri=shown_iri open=set_shown_iri />
            </section>
        </Show>
    }
}

// ------------------------------------------------------------------------------------ home (calendar, to-dos, topics)

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// 0 = Monday ... 6 = Sunday (days-from-civil; 1970-01-01 was a Thursday).
fn weekday(year: i32, month: u32, day: u32) -> usize {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month as i64 + 9) % 12) + 2) / 5 + day as i64 - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    (days + 3).rem_euclid(7) as usize
}

fn split_date(date: &str) -> Option<(i32, u32)> {
    let year = date.get(0..4)?.parse().ok()?;
    let month = date.get(5..7)?.parse().ok()?;
    (1..=12).contains(&month).then_some((year, month))
}

/// The calendar day (YYYY-MM-DD) an event belongs to.
fn event_day(event: &Value) -> Option<String> {
    ["occurred_on", "occurred_at", "started_at"].iter().find_map(|k| event[*k].as_str()).map(|raw| raw.chars().take(10).collect())
}

/// The items about your focus: a name or description that contains one of `config::FOCUS_KEYWORDS`.
fn is_focus(item: &Value) -> bool {
    let hay = format!("{} {}", text(item, "name"), text(item, "description")).to_lowercase();
    crate::config::FOCUS_KEYWORDS.iter().any(|word| hay.contains(&word.to_lowercase()))
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = lifeui)]
    fn store_get(key: &str) -> String;
    #[wasm_bindgen(js_namespace = lifeui)]
    fn store_set(key: &str, value: &str);
}

fn load_done() -> Vec<String> {
    store_get("todo-done").lines().filter(|l| !l.is_empty()).map(String::from).collect()
}

#[component]
fn HomeTab(set_screen: WriteSignal<String>, refresh: RwSignal<u32>, open_agent: Callback<()>) -> impl IntoView {
    let (events, set_events) = signal(Vec::<Value>::new());
    let (tasks, set_tasks) = signal(Vec::<Value>::new());
    let (topics, set_topics) = signal(Vec::<Value>::new());
    let (status, set_status) = signal(String::from(LOADING));
    let done = RwSignal::new(load_done());
    let start = today();
    let (year0, month0) = split_date(&start).unwrap_or((2026, 1));
    let (view_month, set_view_month) = signal((year0, month0));
    let selected = RwSignal::new(start.clone());

    Effect::new(move |_| {
        refresh.track();
        spawn_local(async move {
            let (e, t, i) = (
                api::get("/api/events?limit=200").await,
                api::get("/api/entities?class=Task&limit=200").await,
                api::get("/api/entities?class=Interest&limit=200").await,
            );
            let mut problems = Vec::new();
            for (outcome, key, target) in [(e, "events", set_events), (t, "entities", set_tasks), (i, "entities", set_topics)] {
                match outcome {
                    Outcome::Ok(data) => target.set(data[key].as_array().cloned().unwrap_or_default()),
                    Outcome::Problems(p) => problems.push(p.join(" ")),
                    Outcome::LoggedOut => set_screen.set("login".into()),
                }
            }
            set_status.set(problems.join(" "));
        });
    });

    let is_open = |task: &Value| !matches!(text(task, "status").as_str(), "Completed" | "Cancelled");
    let toggle = move |iri: String| {
        done.update(|list| {
            if let Some(at) = list.iter().position(|d| *d == iri) {
                list.remove(at);
            } else {
                list.push(iri);
            }
            store_set("todo-done", &list.join("\n"));
        });
    };

    // to-dos: open ones first (earliest due date first, then priority), ticked and finished ones at the bottom
    let todo_list = move || {
        let ticked = done.get();
        let mut list: Vec<(bool, Value)> = tasks.get().into_iter().map(|t| (!is_open(&t) || ticked.contains(&text(&t, "iri")), t)).collect();
        list.sort_by_key(|(finished, t)| {
            let due = text(t, "due");
            let priority = t["priority"].as_i64().or_else(|| t["priority"].as_str().and_then(|p| p.parse().ok())).unwrap_or(9);
            (*finished, due.is_empty(), due, priority, text(t, "name"))
        });
        list
    };
    let open_count = move || todo_list().iter().filter(|(finished, _)| !finished).count();

    let shift = move |delta: i32| {
        set_view_month.update(|(y, m)| {
            let index = (*y * 12 + *m as i32 - 1) + delta;
            *y = index.div_euclid(12);
            *m = index.rem_euclid(12) as u32 + 1;
        });
    };

    let calendar = {
        let start = start.clone();
        move || {
            let (year, month) = view_month.get();
            let lead = weekday(year, month, 1);
            let prefix = format!("{year:04}-{month:02}-");
            let (event_list, task_list) = (events.get(), tasks.get());
            let mut cells = (0..lead).map(|_| view! { <span class="cal-blank"></span> }.into_any()).collect::<Vec<_>>();
            for day in 1..=days_in_month(year, month) {
                let date = format!("{prefix}{day:02}");
                let n_events = event_list.iter().filter(|e| event_day(e).as_deref() == Some(date.as_str())).count();
                let n_due = task_list.iter().filter(|t| text(t, "due") == date && is_open(t)).count();
                let (pick, class_date) = (date.clone(), date.clone());
                let label = format!("{day} {} {year}: {n_events} events, {n_due} tasks due", MONTHS[month as usize - 1]);
                let is_today = date == start;
                cells.push(
                    view! {
                        <button class="cal-day" class:today=is_today class:sel=move || selected.get() == class_date
                            aria-label=label aria-pressed=move || (selected.get() == date).to_string()
                            on:click=move |_| selected.set(pick.clone())>
                            <span class="n">{day}</span>
                            <span class="dots" aria-hidden="true">
                                {(n_events > 0).then(|| view! { <i class="dot ev"></i> })}
                                {(n_due > 0).then(|| view! { <i class="dot due"></i> })}
                            </span>
                        </button>
                    }
                    .into_any(),
                );
            }
            cells
        }
    };

    let day_panel = move || {
        let date = selected.get();
        let on_day: Vec<Value> = events.get().into_iter().filter(|e| event_day(e).as_deref() == Some(date.as_str())).collect();
        let due: Vec<Value> = tasks.get().into_iter().filter(|t| text(t, "due") == date && is_open(t)).collect();
        let empty = on_day.is_empty() && due.is_empty();
        view! {
            <div class="day-panel" aria-live="polite">
                <h3>{date}</h3>
                {empty.then(|| view! { <p class="muted">"Nothing on this day."</p> })}
                <ul class="agenda">
                    {on_day.into_iter().map(|e| view! {
                        <li><i class="dot ev"></i><span>{text(&e, "name")}</span><span class="kind">{text(&e, "class")}</span></li>
                    }).collect_view()}
                    {due.into_iter().map(|t| view! {
                        <li><i class="dot due"></i><span>{text(&t, "name")}</span><span class="kind">"due"</span></li>
                    }).collect_view()}
                </ul>
            </div>
        }
    };

    view! {
        <section class="home">
            <div class="screen-head">
                <h2>"Home"</h2>
                <span class="count">{move || open_count()}" to do"</span>
            </div>
            <ListStatus status />
            <crate::news::HomeToday set_screen open_agent />

            <h3>"Calendar"</h3>
            <div class="card cal">
                <div class="cal-head">
                    <button class="icon-btn" aria-label="Previous month" on:click=move |_| shift(-1)><Icon name="back" /></button>
                    <strong aria-live="polite">{move || { let (y, m) = view_month.get(); format!("{} {y}", MONTHS[m as usize - 1]) }}</strong>
                    <button class="icon-btn" aria-label="Next month" on:click=move |_| shift(1)><Icon name="back" class="flip" /></button>
                </div>
                <div class="cal-grid" role="group" aria-label="Month">
                    {WEEKDAYS.iter().map(|d| view! { <span class="cal-wd">{*d}</span> }).collect_view()}
                    {calendar}
                </div>
                <p class="legend muted"><i class="dot ev"></i>"event"<i class="dot due"></i>"task due"</p>
                {day_panel}
            </div>

            <h3>"To-do"</h3>
            <p class="muted">"Open tasks from your knowledge base. Ticks are remembered on this device only."</p>
            <Show when=move || !tasks.get().is_empty() fallback=|| view! { <p class="muted">"No tasks yet."</p> }>
                <For each=todo_list key=|(finished, t)| format!("{}{finished}", text(t, "iri")) children=move |(finished, task): (bool, Value)| {
                    let iri = text(&task, "iri");
                    let due = text(&task, "due");
                    let overdue = !finished && !due.is_empty() && due.as_str() < today().as_str();
                    let focus = is_focus(&task);
                    let click_iri = iri.clone();
                    view! {
                        <article class="card todo" class:finished=finished>
                            <button class="check" role="checkbox" aria-checked=finished.to_string()
                                aria-label=format!("Mark done: {}", text(&task, "name"))
                                disabled=!is_open(&task)
                                on:click=move |_| toggle(click_iri.clone())></button>
                            <div class="todo-body">
                                <strong>{text(&task, "name")}</strong>
                                <div class="row">
                                    {(!due.is_empty()).then(|| view! { <span class="badge" class:s-Secret=overdue>{if overdue { format!("overdue {due}") } else { format!("due {due}") }}</span> })}
                                    {focus.then(|| view! { <span class="badge s-Sensitive">{crate::config::FOCUS_NAME}</span> })}
                                </div>
                            </div>
                        </article>
                    }
                } />
            </Show>

            <h3>{format!("To look into: {}", crate::config::FOCUS_NAME)}</h3>
            <p class="muted">{crate::config::FOCUS_DESCRIPTION}</p>
            {move || {
                let list: Vec<Value> = topics.get().into_iter().filter(is_focus).collect();
                if list.is_empty() {
                    return view! { <p class="muted">{format!("No {} topics found yet.", crate::config::FOCUS_NAME)}</p> }.into_any();
                }
                list.into_iter().map(|topic| view! {
                    <article class="card topic">
                        <strong>{text(&topic, "name")}</strong>
                        <p class="muted">{text(&topic, "description")}</p>
                    </article>
                }).collect_view().into_any()
            }}
        </section>
    }
}

// ------------------------------------------------------------------------------------ agent (inbox, feed, runs)

const MESSAGE_KINDS: [&str; 5] = ["briefing", "overview", "question", "proposal", "note"];

/// The agent's status words reuse the dot styles of the knowledge-base statuses.
fn inbox_dot(status: &str) -> &'static str {
    match status {
        "new" => "Planned",
        "picked_up" => "InProgress",
        "done" => "Completed",
        "needs_answer" => "Blocked",
        _ => "Cancelled",
    }
}

fn snake_label(name: &str) -> String {
    let mut text = name.replace('_', " ");
    if let Some(first) = text.get(0..1) {
        text = first.to_uppercase() + &text[1..];
    }
    text
}

/// "Fri 27 Nov 2026, 15:00" for a proposal's local date and optional time.
fn proposal_when(p: &Value) -> String {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let date = text(p, "date");
    let parts = (date.get(0..4).and_then(|y| y.parse::<i32>().ok()), date.get(5..7).and_then(|m| m.parse::<u32>().ok()), date.get(8..10).and_then(|d| d.parse::<u32>().ok()));
    let mut out = match parts {
        (Some(y), Some(m), Some(d)) if (1..=12).contains(&m) => format!("{} {d} {} {y}", DAYS[weekday(y, m, d)], MONTHS[m as usize - 1]),
        _ => date,
    };
    let time = text(p, "time");
    if !time.is_empty() {
        out.push_str(", ");
        out.push_str(&time);
    }
    out
}

/// The POST /api/events body for an Agent proposal as it stands (the Add button). The owner's wall-clock time becomes an
/// instant in the browser, exactly as the event form does it.
fn proposal_event(p: &Value, inbox_id: &str) -> Option<Value> {
    let mut body = json!({
        "class": text(p, "class"), "name": text(p, "name"), "status": "Planned", "sensitivity": "Personal", "from_proposal": inbox_id,
    });
    let description = text(p, "description");
    if !description.is_empty() {
        body["description"] = json!(description);
    }
    let time = text(p, "time");
    if time.is_empty() {
        body["occurred_on"] = json!(text(p, "date"));
    } else {
        body["occurred_at"] = json!(api::to_instant(&text(p, "date"), &time)?);
    }
    Some(body)
}

#[component]
fn AgentTab(
    set_screen: WriteSignal<String>,
    /// bumped by the event form (Edit of a proposal), so the inbox reloads after it is saved
    events_changed: RwSignal<u32>,
    edit_event: RwSignal<Option<Value>>,
    sheet: SheetCtl,
) -> impl IntoView {
    let (view_name, set_view) = signal("inbox");
    let refresh = RwSignal::new(0u32);
    let (items, set_items) = signal(Vec::<Value>::new());
    let (status, set_status) = signal(String::from(LOADING));
    let (problems, set_problems) = signal(Vec::<String>::new());
    let (busy, set_busy) = signal(false);
    let draft = RwSignal::new(String::new());
    let (test_kind, set_test_kind) = signal(String::from("note"));
    let test_title = RwSignal::new(String::new());
    let test_body = RwSignal::new(String::new());
    let (enabled, set_enabled) = signal(false);
    let token_label = RwSignal::new(String::new());
    let new_token = RwSignal::new(String::new());

    Effect::new(move |_| {
        refresh.track();
        events_changed.track();
        let name = view_name.get();
        set_status.set(LOADING.into());
        spawn_local(async move {
            let answer = api::get(&format!("/api/agent?view={name}&limit=100")).await;
            if view_name.get_untracked() != name {
                return; // the owner switched views while this answer was on its way
            }
            match answer {
                Outcome::Ok(data) => {
                    if name == "settings" {
                        set_enabled.set(data["enabled"].as_bool().unwrap_or(false));
                    }
                    let list = data[if name == "settings" { "tokens" } else { "items" }].as_array().cloned().unwrap_or_default();
                    set_status.set(if list.is_empty() {
                        match name {
                            "feed" => "Nothing from the agent yet. The morning briefing, midday check and evening overview will appear here.".into(),
                            "runs" => "No scheduled runs yet.".into(),
                            "settings" => "No runner tokens yet.".into(),
                            _ => "Your inbox is empty. Tell the agent what to do.".into(),
                        }
                    } else {
                        String::new()
                    });
                    set_items.set(list);
                }
                Outcome::Problems(p) => set_status.set(p.join(" ")),
                Outcome::LoggedOut => set_screen.set("login".into()),
            }
        });
    });

    // one POST helper: on success clear the problems and reload the list
    let send = move |body: Value, on_ok: Box<dyn Fn(Value)>| {
        set_busy.set(true);
        set_problems.set(vec![]);
        spawn_local(async move {
            match api::post("/api/agent", &body).await {
                Outcome::Ok(data) => {
                    on_ok(data);
                    refresh.update(|n| *n += 1);
                }
                Outcome::Problems(p) => set_problems.set(p),
                Outcome::LoggedOut => set_screen.set("login".into()),
            }
            set_busy.set(false);
        });
    };
    let send_inbox = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        send(json!({ "kind": "inbox", "body": draft.get() }), Box::new(move |_| draft.set(String::new())));
    };
    let send_test = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let body = json!({ "kind": "message", "message_kind": test_kind.get(), "title": test_title.get(), "body": test_body.get() });
        send(body, Box::new(move |_| { test_title.set(String::new()); test_body.set(String::new()); }));
    };
    let flip_switch = move |_| {
        let target = !enabled.get_untracked();
        send(json!({ "kind": "switch", "enabled": target }), Box::new(move |_| set_enabled.set(target)));
    };
    let make_token = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        send(
            json!({ "kind": "token_create", "label": token_label.get() }),
            Box::new(move |data| {
                new_token.set(text(&data, "token"));
                token_label.set(String::new());
            }),
        );
    };
    let set_item_status = move |id: String, new_status: &'static str| {
        send(json!({ "kind": "inbox_status", "id": id, "status": new_status }), Box::new(|_| {}));
    };
    // Add: the proposal as it stands becomes an event, through the same checks as the event form
    let add_proposal = move |proposal: Value, id: String| {
        let Some(body) = proposal_event(&proposal, &id) else {
            set_problems.set(vec!["The proposed date or time is not valid: use Edit.".into()]);
            return;
        };
        set_busy.set(true);
        set_problems.set(vec![]);
        spawn_local(async move {
            match api::post("/api/events", &body).await {
                Outcome::Ok(_) => refresh.update(|n| *n += 1),
                Outcome::Problems(p) => set_problems.set(p),
                Outcome::LoggedOut => set_screen.set("login".into()),
            }
            set_busy.set(false);
        });
    };
    let edit_proposal = move |proposal: Value, id: String| {
        edit_event.set(Some(json!({
            "class": text(&proposal, "class"), "name": text(&proposal, "name"), "description": text(&proposal, "description"),
            "status": "Planned", "occurred_on": text(&proposal, "date"), "_time": text(&proposal, "time"), "_from_proposal": id,
        })));
        sheet.show(Sheet::Event);
    };

    let tab_button = move |name: &'static str, label: &'static str| {
        view! {
            <button class=move || if view_name.get() == name { "seg on" } else { "seg" } aria-pressed=move || (view_name.get() == name).to_string()
                on:click=move |_| { set_problems.set(vec![]); set_items.set(vec![]); new_token.set(String::new()); set_view.set(name); }>{label}</button>
        }
    };

    view! {
        <section class="agent">
            <div class="screen-head">
                <h2>"Agent"</h2>
                <span class="count">{move || items.get().len()}" shown"</span>
            </div>
            <p class="muted">"Give the agent tasks and read what it reports. It will run three times a day: morning briefing, midday inbox check, evening overview."</p>
            <div class="segs" role="group" aria-label="Agent views">
                {tab_button("inbox", "Inbox")}
                {tab_button("feed", "Feed")}
                {tab_button("runs", "Runs")}
                {tab_button("settings", "Settings")}
            </div>

            <Show when=move || view_name.get() == "inbox">
                <form on:submit=send_inbox>
                    <label>"Tell the agent what to do"
                        <textarea rows="3" maxlength="2000" required placeholder="e.g. Add dentist on Friday at 15:00"
                            prop:value=move || draft.get() on:input=move |ev| draft.set(event_target_value(&ev))></textarea>
                    </label>
                    <p class="warn">"Never write passwords, PINs or ID numbers here."</p>
                    <Problems messages=problems />
                    <button type="submit" class="btn" prop:disabled=move || busy.get()>{move || if busy.get() { "Sending..." } else { "Send to agent" }}</button>
                </form>
            </Show>

            <Show when=move || view_name.get() == "settings">
                <div class="card switch-card">
                    <div>
                        <strong>"Agent"</strong>
                        <p class="muted">{move || if enabled.get() { "ON: the scheduled runs may work." } else { "OFF: every call from the runner is refused." }}</p>
                    </div>
                    <button class="switch" role="switch" aria-label="Agent on or off" aria-checked=move || enabled.get().to_string()
                        prop:disabled=move || busy.get() on:click=flip_switch><span class="knob"></span></button>
                </div>
                <h3>"Runner tokens"</h3>
                <p class="muted">"The scheduled agent signs in with a token. A token can only use the runner API: it cannot read events or your notes."</p>
                <Show when=move || !new_token.get().is_empty()>
                    <div class="card token-once" role="status">
                        <p class="warn">"Copy this token now. It is never shown again."</p>
                        <code>{move || new_token.get()}</code>
                    </div>
                </Show>
                <form on:submit=make_token>
                    <label>"Name for the new token"
                        <input type="text" maxlength="60" required placeholder="e.g. my pc" prop:value=move || token_label.get() on:input=move |ev| token_label.set(event_target_value(&ev)) />
                    </label>
                    <Problems messages=problems />
                    <button type="submit" class="btn secondary" prop:disabled=move || busy.get()>"Create token"</button>
                </form>
            </Show>

            <ListStatus status />
            <For each=move || items.get() key=|i| format!("{}{}{}{}{}", text(i, "id"), text(i, "status"), text(i, "reply"), text(i, "revoked_at"), text(i, "proposal_status")) children=move |item: Value| {
                let name = view_name.get_untracked();
                let id = text(&item, "id");
                match name {
                    "feed" => view! {
                        <article class="card">
                            <div class="row">
                                <span class="when">{short_time(&text(&item, "created_at"))}</span>
                                <span class="kind">{snake_label(&text(&item, "kind"))}</span>
                                <span class="badge">{text(&item, "slot")}</span>
                                {(text(&item, "origin") == "manual").then(|| view! { <span class="badge">"test"</span> })}
                            </div>
                            <strong>{text(&item, "title")}</strong>
                            <p class="msg">{text(&item, "body")}</p>
                        </article>
                    }.into_any(),
                    "settings" => {
                        let revoked = !text(&item, "revoked_at").is_empty();
                        let revoke_id = id.clone();
                        let used = text(&item, "last_used_at");
                        view! {
                            <article class="card" class:finished=revoked>
                                <div class="row">
                                    <span class="when">{short_time(&text(&item, "created_at"))}</span>
                                    {revoked.then(|| view! { <span class="badge s-Secret">"revoked"</span> })}
                                </div>
                                <strong>{text(&item, "label")}</strong>
                                <p class="muted">{if used.is_empty() { "Never used".to_string() } else { format!("Last used {}", short_time(&used)) }}</p>
                                {(!revoked).then(|| view! {
                                    <div class="actions-inline">
                                        <button class="btn secondary sm" on:click=move |_| send(json!({ "kind": "token_revoke", "id": revoke_id.clone() }), Box::new(|_| {}))>"Revoke"</button>
                                    </div>
                                })}
                            </article>
                        }.into_any()
                    }
                    "runs" => view! {
                        <article class="card">
                            <div class="row">
                                <span class="when">{short_time(&text(&item, "started_at"))}</span>
                                <span class="kind">{text(&item, "slot")}</span>
                                <span class=format!("badge {}", if text(&item, "status") == "failed" { "s-Secret" } else { "" })>{text(&item, "status")}</span>
                            </div>
                            <p class="muted">{format!("{} tool calls, {} tokens", show_value(&item["tool_calls"]), show_value(&item["tokens"]))}</p>
                            <p class="msg">{text(&item, "summary")}</p>
                        </article>
                    }.into_any(),
                    _ => {
                        let state = text(&item, "status");
                        let open = !matches!(state.as_str(), "done" | "cancelled");
                        let (done_id, cancel_id, reopen_id) = (id.clone(), id.clone(), id.clone());
                        let reply = text(&item, "reply");
                        let proposal = item["proposal"].clone();
                        let proposal_state = text(&item, "proposal_status");
                        let proposal_view = (proposal.is_object()).then(|| {
                            let (p_add, p_edit) = (proposal.clone(), proposal.clone());
                            let (add_id, edit_id, dismiss_id) = (id.clone(), id.clone(), id.clone());
                            let pending = proposal_state == "pending";
                            let label = match proposal_state.as_str() {
                                "added" => "Added to your calendar",
                                "dismissed" => "Proposal dismissed",
                                _ => "Proposed event",
                            };
                            let description = text(&proposal, "description");
                            view! {
                                <div class="proposal" class:pending=pending>
                                    <div class="row">
                                        <span class="kind">{label}</span>
                                        <span class="badge">{text(&proposal, "class")}</span>
                                    </div>
                                    <strong>{text(&proposal, "name")}</strong>
                                    <p class="when-line">{proposal_when(&proposal)}</p>
                                    {(!description.is_empty()).then(|| view! { <p class="muted">{description.clone()}</p> })}
                                    {pending.then(|| view! {
                                        <div class="actions-inline">
                                            <button class="btn sm" prop:disabled=move || busy.get() on:click=move |_| add_proposal(p_add.clone(), add_id.clone())>"Add"</button>
                                            <button class="btn secondary sm" prop:disabled=move || busy.get() on:click=move |_| edit_proposal(p_edit.clone(), edit_id.clone())>"Edit"</button>
                                            <button class="btn ghost sm" prop:disabled=move || busy.get()
                                                on:click=move |_| send(json!({ "kind": "proposal_dismiss", "id": dismiss_id.clone() }), Box::new(|_| {}))>"Dismiss"</button>
                                        </div>
                                    })}
                                </div>
                            }
                        });
                        let has_pending = proposal_state == "pending";
                        view! {
                            <article class="card" class:finished=!open>
                                <div class="row">
                                    <span class="when">{short_time(&text(&item, "created_at"))}</span>
                                    <span class=format!("status st-{}", inbox_dot(&state))>{snake_label(&state)}</span>
                                </div>
                                <p class="msg">{text(&item, "body")}</p>
                                {(!reply.is_empty()).then(|| view! { <p class="muted">"Agent: " {reply.clone()}</p> })}
                                {proposal_view}
                                <div class="actions-inline">
                                    {(open && !has_pending).then(|| view! {
                                        <button class="btn secondary sm" on:click=move |_| set_item_status(done_id.clone(), "done")>"Mark done"</button>
                                        <button class="btn ghost sm" on:click=move |_| set_item_status(cancel_id.clone(), "cancelled")>"Cancel"</button>
                                    })}
                                    {(!open).then(|| view! {
                                        <button class="btn ghost sm" on:click=move |_| set_item_status(reopen_id.clone(), "new")>"Reopen"</button>
                                    })}
                                </div>
                            </article>
                        }.into_any()
                    }
                }
            } />
            <Show when=move || !matches!(view_name.get(), "inbox" | "settings")><Problems messages=problems /></Show>

            <Show when=move || view_name.get() == "feed">
                <details class="card testbox">
                    <summary>"Write a test message (until the agent runs by itself)"</summary>
                    <form on:submit=send_test>
                        <label>"Kind"
                            <select prop:value=move || test_kind.get() on:change=move |ev| set_test_kind.set(event_target_value(&ev))>
                                <Options items=&MESSAGE_KINDS />
                            </select>
                        </label>
                        <label>"Title"
                            <input type="text" maxlength="200" required prop:value=move || test_title.get() on:input=move |ev| test_title.set(event_target_value(&ev)) />
                        </label>
                        <label>"Message"
                            <textarea rows="3" maxlength="8000" required prop:value=move || test_body.get() on:input=move |ev| test_body.set(event_target_value(&ev))></textarea>
                        </label>
                        <button type="submit" class="btn secondary" prop:disabled=move || busy.get()>"Add to feed"</button>
                    </form>
                </details>
                // The old "Tools" tab (API, database and passkey checks) lives here since News took its place.
                <details class="diagnostics">
                    <summary>"Diagnostics"</summary>
                    <crate::Diagnostics />
                </details>
            </Show>
        </section>
    }
}

// ------------------------------------------------------------------------------------ shell

#[component]
pub fn Home(set_screen: WriteSignal<String>) -> impl IntoView {
    let (tab, set_tab) = signal("home");
    let refresh = RwSignal::new(0u32);
    let edit_event = RwSignal::new(None::<Value>);
    let sheet_ctl = SheetCtl::new();

    // the native dialog follows the signal (the animation is CSS; see lifeui.sheet in public/lifeui.js)
    Effect::new(move |_| sheet(sheet_ctl.open.get()));

    let go = move |name: &'static str| {
        move |_| {
            edit_event.set(None);
            set_tab.set(name);
        }
    };
    let nav = move |name: &'static str, label: &'static str| {
        view! {
            <button class=move || if tab.get() == name { "tab on" } else { "tab" }
                aria-current=move || if tab.get() == name { "page" } else { "false" } on:click=go(name)>
                <Icon name=name />
                <span>{label}</span>
            </button>
        }
    };
    let add = move |_| {
        edit_event.set(None);
        sheet_ctl.show(Sheet::Event);
    };
    let open_agent = Callback::new(move |_: ()| set_tab.set("agent"));

    view! {
        {move || match tab.get() {
            "home" => view! { <HomeTab set_screen refresh open_agent /> }.into_any(),
            "agent" => view! { <AgentTab set_screen events_changed=refresh edit_event sheet=sheet_ctl /> }.into_any(),
            "timeline" => view! { <Timeline set_screen refresh edit_event sheet=sheet_ctl /> }.into_any(),
            "explore" => view! { <Explore set_screen /> }.into_any(),
            _ => view! { <crate::news::NewsTab set_screen open_agent /> }.into_any(),
        }}
        <Show when=move || matches!(tab.get(), "home" | "timeline")>
            <button class="fab" on:click=add>
                <Icon name="plus" />
                "Add event"
            </button>
        </Show>
        <nav class="tabs" aria-label="Sections">
            {nav("home", "Home")}
            {nav("timeline", "Timeline")}
            {nav("explore", "Explore")}
            {nav("news", "News")}
            {nav("agent", "Agent")}
        </nav>
        <dialog id="sheet" class="sheet"
            aria-label="Event"
            on:cancel=move |ev: leptos::ev::Event| { ev.prevent_default(); sheet_ctl.hide(); }
            on:click=move |ev: leptos::ev::MouseEvent| { if ev.target() == ev.current_target() { sheet_ctl.hide(); } }>
            <div class="sheet-grab" aria-hidden="true"></div>
            <div class="sheet-body">
                {move || {
                    sheet_ctl.generation.track();
                    match sheet_ctl.kind.get() {
                        Sheet::Event => view! { <EventForm set_screen refresh edit_event sheet=sheet_ctl /> }.into_any(),
                        Sheet::Closed => ().into_any(),
                    }
                }}
            </div>
        </dialog>
    }
}
