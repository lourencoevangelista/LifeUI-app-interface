//! The News tab: the daily news briefing about your interests, published by the home server (see tools/news/ and
//! POST /api/runner/news). Top to bottom: the Today card (weather hints, calendar, top 3, inbox), the weather, the
//! "3 things that matter today" intro, a tag filter, the items (read / save / dismiss; save also files a reading-list task),
//! the monthly feed-tuning report, the server status with the "Stay awake tonight" switch, and a date picker.
//! The Weekly view shows Sunday's digest: trends, then the week's top items with the Focus highlights marked.
//!
//! Item links come from feeds the server does not control: they are only ever rendered as http(s) links that open
//! in a new tab with rel="noopener noreferrer" (the API refuses anything else anyway).

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

use crate::api::{self, Outcome};
use crate::config::{ASK_NEWS_URL, FOCUS_NAME, SERVER_NAME};
use crate::ui::{Icon, ListStatus, LOADING, Problems, short_time, text};

/// (label, which item tags count)
const FILTERS: [(&str, &[&str]); 5] = [
    ("All", &[]),
    ("AI", &["ai_breakthrough", "industry"]),
    ("Research", &["research"]),
    ("Releases", &["release"]),
    (FOCUS_NAME, &["focus_relevant"]),
];

fn tags(item: &Value) -> Vec<String> {
    item["tags"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn matches_filter(item: &Value, filter: &str) -> bool {
    let wanted = FILTERS.iter().find(|(label, _)| *label == filter).map(|(_, t)| *t).unwrap_or(&[]);
    wanted.is_empty() || tags(item).iter().any(|t| wanted.contains(&t.as_str()))
}

fn tag_label(tag: &str) -> &'static str {
    match tag {
        "ai_breakthrough" => "Breakthrough",
        "release" => "Release",
        "research" => "Research",
        "focus_relevant" => FOCUS_NAME,
        _ => "Industry",
    }
}

fn safe_url(url: &str) -> Option<String> {
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_string())
}

fn num(value: &Value) -> Option<f64> {
    value.as_f64()
}

fn degrees(value: &Value) -> String {
    num(value).map(|v| format!("{v:.0}°")).unwrap_or_else(|| "–".into())
}

/// "2026-11-19T09:05:12+00:00" -> "09:05"
fn clock(raw: &str) -> String {
    raw.get(11..16).unwrap_or("").to_string()
}

fn list(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

fn strs(value: &Value) -> Vec<String> {
    value.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Shown at the top of the News tab and on Home: is today's briefing there, the weather hints (IPMA warnings as a
/// banner), today's Google Calendar + LifeUI events, the top 3 items and the open inbox count.
/// `today` is the `today` block of GET /api/news, `briefing` the newest daily briefing (ignored unless it is today's).
/// `show_missing`: show the "No briefing today" banner (Home only; the News tab leaves it out).
#[component]
pub fn TodayCard(today: Value, briefing: Value, open_agent: Callback<()>, #[prop(optional)] show_missing: bool) -> impl IntoView {
    if today.is_null() {
        return ().into_any();
    }
    let missing = show_missing && today["missing"].as_bool().unwrap_or(false);
    let is_today = text(&briefing, "type") == "daily" && !text(&today, "date").is_empty() && text(&briefing, "date") == text(&today, "date");
    let b = if is_today { briefing } else { Value::Null };
    let weather = b["weather"].clone();
    let line = Some(text(&weather, "line")).filter(|l| !l.is_empty()).unwrap_or_else(|| text(&weather, "summary"));
    let hints = list(&weather["hints"]);
    let (alerts, tips): (Vec<Value>, Vec<Value>) = hints.into_iter().partition(|h| text(h, "level") == "alert");
    // calendar: Google Calendar (from the briefing) + LifeUI events (live), all-day first, then by time
    let mut agenda: Vec<(String, String)> = list(&b["extras"]["calendar"]).iter().map(|e| (text(e, "time"), text(e, "title"))).collect();
    agenda.extend(list(&today["events"]).iter().map(|e| (text(e, "time"), text(e, "name"))));
    agenda.sort();
    agenda.dedup();
    let top: Vec<Value> = list(&b["items"]).into_iter().filter(|i| i["dismissed_at"].is_null()).take(3).collect();
    let inbox = today["inbox_open"].as_i64().unwrap_or(0);
    view! {
        <article class="card today" aria-label="Today">
            <div class="today-head">
                <strong>"Today"</strong>
                <span class="muted">{text(&today, "date")}</span>
            </div>
            {missing.then(|| view! {
                <p class="warn" role="alert">{format!("No briefing today: {SERVER_NAME} didn't run its morning task, or the task failed. Check the server.")}</p>
            })}
            {alerts.into_iter().map(|h| view! { <p class="warning-badge w-orange" role="alert">{text(&h, "text")}</p> }).collect_view()}
            {(!line.is_empty()).then(|| view! { <p class="weather-line"><Icon name="sun" />" "{line}</p> })}
            {(!tips.is_empty()).then(|| view! {
                <ul class="hints">{tips.into_iter().map(|h| view! { <li class=format!("hint h-{}", text(&h, "id"))>{text(&h, "text")}</li> }).collect_view()}</ul>
            })}
            <h4>"Calendar"</h4>
            {if agenda.is_empty() {
                view! { <p class="muted">"Nothing in the calendar today."</p> }.into_any()
            } else {
                view! {
                    <ul class="agenda">
                        {agenda.into_iter().map(|(time, title)| view! {
                            <li><span class="when">{if time.is_empty() { "all day".to_string() } else { time }}</span>" "{title}</li>
                        }).collect_view()}
                    </ul>
                }.into_any()
            }}
            {(!top.is_empty()).then(|| view! {
                <h4>"Top 3"</h4>
                <ol class="top3">
                    {top.into_iter().map(|i| match safe_url(&text(&i, "url")) {
                        Some(href) => view! { <li><a href=href target="_blank" rel="noopener noreferrer">{text(&i, "title")}</a></li> }.into_any(),
                        None => view! { <li>{text(&i, "title")}</li> }.into_any(),
                    }).collect_view()}
                </ol>
            })}
            <button class="btn secondary sm inbox-link" on:click=move |_| open_agent.run(())>
                {match inbox { 0 => "Inbox: nothing open".to_string(), 1 => "1 open inbox item →".to_string(), n => format!("{n} open inbox items →") }}
            </button>
        </article>
    }
    .into_any()
}

/// Home: the same Today card, loaded on its own.
#[component]
pub fn HomeToday(set_screen: WriteSignal<String>, open_agent: Callback<()>) -> impl IntoView {
    let (data, set_data) = signal(Value::Null);
    spawn_local(async move {
        match api::get("/api/news").await {
            Outcome::Ok(d) => set_data.set(d),
            Outcome::Problems(_) => {} // Home still works without it
            Outcome::LoggedOut => set_screen.set("login".into()),
        }
    });
    move || {
        let d = data.get();
        view! { <TodayCard today=d["today"].clone() briefing=d["briefing"].clone() open_agent show_missing=true /> }
    }
}

/// The monthly reweighting report (feedback job on the 1st), shown with that day's briefing.
#[component]
fn FeedbackReport(feedback: Value) -> impl IntoView {
    let changes = list(&feedback["changes"]);
    view! {
        <details class="card feedback">
            <summary>{format!("Feed tuning · {}", text(&feedback, "month"))}</summary>
            <p class="muted">{format!("From your reads, saves and dismissals over {} items. Bounded steps; the previous weights are kept on the server ({}).",
                feedback["items"].as_i64().unwrap_or(0), text(&feedback, "previous"))}</p>
            {if changes.is_empty() {
                view! { <p class="muted">"No changes this month."</p> }.into_any()
            } else {
                view! {
                    <ul class="changes">
                        {changes.into_iter().map(|c| view! {
                            <li>
                                <span class="badge">{text(&c, "kind")}</span>" "<strong>{text(&c, "name")}</strong>
                                {format!(" {} → {}", c["from"], c["to"])}
                                <span class="muted">{format!(" · {}", text(&c, "reason"))}</span>
                            </li>
                        }).collect_view()}
                    </ul>
                }.into_any()
            }}
        </details>
    }
}

/// The weekly digest's trends.
#[component]
fn Trends(digest: Value) -> impl IntoView {
    let trends = list(&digest["trends"]);
    (!trends.is_empty()).then(|| view! {
        <article class="card trends">
            <h4>"Trends this week"</h4>
            {trends.into_iter().map(|t| view! { <p><strong>{text(&t, "title")}</strong>" "{text(&t, "text")}</p> }).collect_view()}
        </article>
    })
}

#[component]
fn WeatherCard(weather: Value) -> impl IntoView {
    if weather.is_null() {
        return view! { <article class="card weather"><p class="muted">"No weather in this briefing."</p></article> }.into_any();
    }
    let stale = weather["stale"].as_bool().unwrap_or(false);
    let hourly = weather["hourly"].as_array().cloned().unwrap_or_default();
    let warnings: Vec<Value> = weather["warnings"].as_array().cloned().unwrap_or_default().into_iter().filter(|w| w["today"].as_bool().unwrap_or(true)).collect();
    let rain = num(&weather["precip_prob"]).map(|p| format!("{p:.0}% rain")).unwrap_or_default();
    let wind = format!("{} {}", text(&weather["wind"], "class"), text(&weather["wind"], "dir")).trim().to_string();
    let updated = clock(&text(&weather, "fetched_at"));
    view! {
        <article class="card weather" aria-label="Weather">
            <div class="weather-top">
                <Icon name="sun" />
                <div class="weather-temps">
                    <strong>{format!("{} – {}", degrees(&weather["tmin"]), degrees(&weather["tmax"]))}</strong>
                    <span class="muted">{Some(text(&weather, "weather_type")).filter(|t| !t.is_empty()).unwrap_or_else(|| text(&weather, "weather_type_pt"))}</span>
                </div>
                <div class="weather-side">
                    <span>{rain}</span>
                    <span class="muted">{wind}</span>
                </div>
            </div>
            {warnings.into_iter().map(|w| {
                let level = text(&w, "level");
                view! { <p class=format!("warning-badge w-{level}")>{format!("{} warning: {}", level, text(&w, "type").to_lowercase())}</p> }
            }).collect_view()}
            <p class="weather-line">{text(&weather, "summary")}</p>
            {(!hourly.is_empty()).then(|| view! {
                <ol class="hourly" aria-label="Hour by hour, 09:00 to 21:00">
                    {hourly.into_iter().map(|h| {
                        let rain = num(&h["precip_prob"]).filter(|p| *p > 0.0).map(|p| format!("{p:.0}%")).unwrap_or_default();
                        view! {
                            <li>
                                <span class="h">{text(&h, "time").get(0..2).unwrap_or("").to_string()}</span>
                                <span class="t">{degrees(&h["temp"])}</span>
                                <span class="r">{rain}</span>
                            </li>
                        }
                    }).collect_view()}
                </ol>
            })}
            <p class="source muted">
                "IPMA"{(!updated.is_empty()).then(|| format!(" · updated {updated}"))}
                {stale.then(|| view! { <span class="badge stale">"stale"</span> })}
            </p>
        </article>
    }
    .into_any()
}

#[component]
fn ServerStatus(status: Value, stay_on: ReadSignal<bool>, busy: ReadSignal<bool>, flip: impl Fn() + Copy + 'static) -> impl IntoView {
    let alerts: Vec<String> = status["alerts"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let failed: Vec<String> = status["failed_steps"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let updates = status["updates_applied"].as_array().map_or(0, Vec::len);
    let disks = status["disk_usage_pct"].as_object().map(|o| o.iter().map(|(k, v)| format!("{k} {}%", v.as_i64().unwrap_or(0))).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    let backup = status["last_backup"]["age_hours"].as_f64().map(|h| format!("{h:.0} h ago")).unwrap_or_else(|| "none".into());
    let gpu = status["gpu_temp_c"].as_i64().map(|t| format!("{t} °C")).unwrap_or_else(|| "–".into());
    let rows: Vec<(&'static str, String)> = if status.is_null() {
        vec![]
    } else {
        vec![
            ("Updates", format!("{updates} applied{}", if status["rebooted"].as_bool().unwrap_or(false) { ", rebooted" } else { "" })),
            ("Disks", disks),
            ("Last backup", backup),
            ("GPU", gpu),
            ("Failed steps", if failed.is_empty() { "none".into() } else { failed.join(", ") }),
        ]
    };
    let attention = !alerts.is_empty() || !failed.is_empty();
    view! {
        <details class="card server-status" open=attention>
            <summary>
                "Server status"
                {attention.then(|| view! { <span class="badge s-Secret">"check"</span> })}
            </summary>
            {alerts.into_iter().map(|a| view! { <p class="warn">{a}</p> }).collect_view()}
            {(!failed.is_empty()).then(|| view! { <p class="warn">{format!("Morning task: {} failed. See the morning task's log on {SERVER_NAME}.", failed.join(", "))}</p> })}
            {if rows.is_empty() {
                view! { <p class="muted">"No server status in this briefing."</p> }.into_any()
            } else {
                view! {
                    <dl class="kv">
                        {rows.into_iter().map(|(k, v)| view! { <dt>{k}</dt><dd>{v}</dd> }).collect_view()}
                    </dl>
                }.into_any()
            }}
            <div class="switch-card stay-awake">
                <div>
                    <strong>"Stay awake tonight"</strong>
                    <p class="muted">{format!("Keeps {SERVER_NAME} on tonight (it skips its evening shutdown).")}</p>
                </div>
                <button class="switch" role="switch" aria-label="Stay awake tonight" aria-checked=move || stay_on.get().to_string()
                    prop:disabled=move || busy.get() on:click=move |_| flip()><span class="knob"></span></button>
            </div>
        </details>
    }
}

#[component]
pub fn NewsTab(set_screen: WriteSignal<String>, open_agent: Callback<()>) -> impl IntoView {
    let refresh = RwSignal::new(0u32);
    let date = RwSignal::new(String::new()); // "" = newest
    let kind = RwSignal::new("daily"); // daily | weekly
    let (briefing, set_briefing) = signal(Value::Null);
    let (today, set_today) = signal(Value::Null);
    let (dates, set_dates) = signal(Vec::<String>::new());
    let (status, set_status) = signal(String::from(LOADING));
    let (problems, set_problems) = signal(Vec::<String>::new());
    let (busy, set_busy) = signal(false);
    let (stay_on, set_stay_on) = signal(false);
    let filter = RwSignal::new("All");
    let show_dismissed = RwSignal::new(false);

    Effect::new(move |_| {
        refresh.track();
        let wanted = date.get();
        let k = kind.get();
        spawn_local(async move {
            let path = if wanted.is_empty() { format!("/api/news?type={k}") } else { format!("/api/news?type={k}&date={wanted}") };
            match api::get(&path).await {
                Outcome::Ok(data) => {
                    set_today.set(data["today"].clone());
                    set_dates.set(data["dates"].as_array().map(|a| a.iter().filter_map(|d| d.as_str().map(String::from)).collect()).unwrap_or_default());
                    set_stay_on.set(data["stay_awake"]["tonight"].as_bool().unwrap_or(false));
                    set_status.set(if data["briefing"].is_null() {
                        if k == "weekly" { format!("No weekly digest yet. {SERVER_NAME} writes one on Sunday mornings.") } else { format!("No briefing yet. {SERVER_NAME} publishes one every morning.") }
                    } else {
                        String::new()
                    });
                    set_briefing.set(data["briefing"].clone());
                }
                Outcome::Problems(p) => set_status.set(p.join(" ")),
                Outcome::LoggedOut => set_screen.set("login".into()),
            }
        });
    });

    let post = move |body: Value, on_ok: Box<dyn Fn(Value)>| {
        set_busy.set(true);
        set_problems.set(vec![]);
        spawn_local(async move {
            match api::post("/api/news", &body).await {
                Outcome::Ok(data) => on_ok(data),
                Outcome::Problems(p) => set_problems.set(p),
                Outcome::LoggedOut => set_screen.set("login".into()),
            }
            set_busy.set(false);
        });
    };
    // Apply an item action and patch that item in place (no reload, the list doesn't jump).
    let act = move |id: String, action: &'static str| {
        post(
            json!({ "kind": "item", "id": id, "action": action }),
            Box::new(move |data| {
                let updated = data["item"].clone();
                set_briefing.update(|b| {
                    if let Some(items) = b["items"].as_array_mut() {
                        for item in items.iter_mut() {
                            if item["id"] == updated["id"] {
                                *item = updated.clone();
                            }
                        }
                    }
                });
            }),
        );
    };
    let flip_stay = move || {
        let target = !stay_on.get_untracked();
        post(json!({ "kind": "stay_awake", "on": target }), Box::new(move |data| set_stay_on.set(data["stay_awake"]["tonight"].as_bool().unwrap_or(target))));
    };

    let visible = move || {
        let f = filter.get();
        let dismissed = show_dismissed.get();
        briefing.get()["items"].as_array().cloned().unwrap_or_default().into_iter()
            .filter(|i| matches_filter(i, f) && (dismissed || i["dismissed_at"].is_null()))
            .collect::<Vec<_>>()
    };
    let hidden_count = move || briefing.get()["items"].as_array().map_or(0, |a| a.iter().filter(|i| !i["dismissed_at"].is_null()).count());
    let weekly = move || kind.get() == "weekly";
    let switch_kind = move |k: &'static str| {
        if kind.get_untracked() != k {
            date.set(String::new());
            set_status.set(LOADING.into());
            set_briefing.set(Value::Null);
            kind.set(k);
        }
    };

    view! {
        <section class="news">
            <div class="screen-head">
                <h2>"News"</h2>
                <label class="date-pick">
                    <span class="sr-only">"Briefing date"</span>
                    <select prop:value=move || date.get() on:change=move |ev| date.set(event_target_value(&ev))>
                        <option value="">"Latest"</option>
                        {move || dates.get().into_iter().map(|d| view! { <option value=d.clone()>{d.clone()}</option> }).collect_view()}
                    </select>
                </label>
            </div>
            <div class="segs views" role="group" aria-label="Daily or weekly">
                {[("daily", "Daily"), ("weekly", "Weekly")].into_iter().map(|(k, label)| view! {
                    <button class=move || if kind.get() == k { "seg on" } else { "seg" } aria-pressed=move || (kind.get() == k).to_string()
                        on:click=move |_| switch_kind(k)>{label}</button>
                }).collect_view()}
            </div>
            <Show when=move || !weekly() && date.get().is_empty()>
                {move || view! { <TodayCard today=today.get() briefing=briefing.get() open_agent /> }}
            </Show>
            <ListStatus status />
            <Problems messages=problems />
            <Show when=move || !briefing.get().is_null()>
                {move || {
                    let b = briefing.get();
                    let feedback = b["extras"]["feedback"].clone();
                    if text(&b, "type") == "weekly" {
                        return view! {
                            <p class="muted briefing-date">{format!("Week ending {} · {}", text(&b, "date"), text(&b, "generated_by_model"))}</p>
                            {(!text(&b, "intro").is_empty()).then(|| view! { <article class="card intro"><p class="msg">{text(&b, "intro")}</p></article> })}
                            <Trends digest=b["extras"]["digest"].clone() />
                        }.into_any();
                    }
                    view! {
                        <p class="muted briefing-date">{format!("{} · {}", text(&b, "date"), text(&b, "generated_by_model"))}</p>
                        <WeatherCard weather=b["weather"].clone() />
                        {(!text(&b, "intro").is_empty()).then(|| view! { <article class="card intro"><p class="msg">{text(&b, "intro")}</p></article> })}
                        {feedback.is_object().then(|| view! { <FeedbackReport feedback=feedback.clone() /> })}
                    }.into_any()
                }}
                <div class="segs filters" role="group" aria-label="Filter news">
                    {FILTERS.iter().map(|(label, _)| {
                        let label: &'static str = label;
                        view! {
                            <button class=move || if filter.get() == label { "seg on" } else { "seg" } aria-pressed=move || (filter.get() == label).to_string()
                                on:click=move |_| filter.set(label)>{label}</button>
                        }
                    }).collect_view()}
                </div>
                <For each=visible key=|i| format!("{}{}{}{}", text(i, "id"), text(i, "read_at"), text(i, "saved_at"), text(i, "dismissed_at")) children=move |item: Value| {
                    let id = text(&item, "id");
                    let (id_read, id_save, id_dismiss) = (id.clone(), id.clone(), id.clone());
                    let read = !item["read_at"].is_null();
                    let saved = !item["saved_at"].is_null();
                    let dismissed = !item["dismissed_at"].is_null();
                    let link = safe_url(&text(&item, "url"));
                    let id_open = id.clone();
                    let when = text(&item, "published_at");
                    let why = text(&item, "why_it_matters");
                    let highlight = strs(&briefing.get_untracked()["extras"]["digest"]["highlights"]).contains(&text(&item, "url"));
                    view! {
                        <article class="card news-item" class:read=read class:finished=dismissed>
                            <div class="row">
                                <span class="kind">{text(&item, "source")}</span>
                                {(!when.is_empty()).then(|| view! { <span class="when">{short_time(&when)}</span> })}
                                {tags(&item).into_iter().map(|t| view! { <span class=format!("badge tag-{t}")>{tag_label(&t)}</span> }).collect_view()}
                                {highlight.then(|| view! { <span class="badge tag-focus_relevant">"Focus highlight"</span> })}
                            </div>
                            {match link {
                                Some(href) => view! {
                                    <a class="news-title" href=href target="_blank" rel="noopener noreferrer"
                                        on:click=move |_| if !read { act(id_open.clone(), "read") }>{text(&item, "title")}</a>
                                }.into_any(),
                                None => view! { <strong>{text(&item, "title")}</strong> }.into_any(),
                            }}
                            <p class="msg">{text(&item, "summary")}</p>
                            {(!why.is_empty()).then(|| view! { <p class="why"><span class="muted">"Why it matters: "</span>{why}</p> })}
                            <div class="actions-inline">
                                <button class="btn secondary sm" prop:disabled=move || busy.get()
                                    on:click=move |_| act(id_read.clone(), if read { "unread" } else { "read" })>{if read { "Mark unread" } else { "Mark read" }}</button>
                                <button class="btn secondary sm" aria-pressed=saved.to_string() prop:disabled=move || busy.get()
                                    title=if saved { "Saved to your reading list (a task on Home)" } else { "Save to your reading list (adds a task on Home)" }
                                    on:click=move |_| act(id_save.clone(), if saved { "unsave" } else { "save" })>{if saved { "Saved ✓" } else { "Save" }}</button>
                                <button class="btn secondary sm" prop:disabled=move || busy.get()
                                    on:click=move |_| act(id_dismiss.clone(), if dismissed { "undismiss" } else { "dismiss" })>{if dismissed { "Restore" } else { "Dismiss" }}</button>
                            </div>
                        </article>
                    }
                } />
                <Show when=move || hidden_count() != 0>
                    <button class="btn secondary sm show-dismissed" on:click=move |_| show_dismissed.update(|v| *v = !*v)>
                        {move || if show_dismissed.get() { "Hide dismissed".to_string() } else { format!("Show {} dismissed", hidden_count()) }}
                    </button>
                </Show>
                <Show when=move || !weekly()>
                    {move || view! { <ServerStatus status=briefing.get()["server_status"].clone() stay_on busy flip=flip_stay /> }}
                </Show>
            </Show>
            <Show when=move || !weekly() && briefing.get().is_null() && status.get() != LOADING>
                // the switch must work even before the first briefing exists
                <ServerStatus status=Value::Null stay_on busy flip=flip_stay />
            </Show>
            <p class="muted news-foot">
                {format!("Built every morning on {SERVER_NAME} from the sources in tools/news/sources.yaml.")}
                {ASK_NEWS_URL.map(|url| view! {
                    " Ask the briefing: " <a href=url target="_blank" rel="noopener noreferrer">{url}</a>
                })}
            </p>
        </section>
    }
}
