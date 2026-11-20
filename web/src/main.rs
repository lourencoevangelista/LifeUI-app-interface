//! LifeUI frontend (Leptos, client-side rendered).
//!
//! Screens: loading -> first-time setup (choose the password) | login | the app.
//! The browser never sees password hashes or tokens: the session lives in an HttpOnly cookie set by
//! the Rust functions in api/. All network calls go through the JS glue in `public/lifeui.js`, which
//! returns plain strings ("" = success, otherwise a message safe to show).

mod api;
mod config;
mod news;
mod ui;

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = lifeui, catch)]
    async fn auth_state() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = lifeui, catch)]
    async fn auth_register(setup_code: &str, password: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = lifeui, catch)]
    async fn auth_login(password: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = lifeui, catch)]
    async fn auth_logout() -> Result<JsValue, JsValue>;

    // Spike helpers (kept for the Phase 1 checks).
    #[wasm_bindgen(js_namespace = lifeui, catch)]
    async fn call_api(path: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = lifeui, js_name = passkeyCreate, catch)]
    async fn passkey_create() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = lifeui, js_name = passkeyLogin, catch)]
    async fn passkey_login() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = lifeui)]
    fn environment() -> String;
}

fn show(result: Result<JsValue, JsValue>) -> String {
    match result {
        Ok(value) => value.as_string().unwrap_or_else(|| "<non-string result>".into()),
        Err(error) => format!("JS error: {error:?}"),
    }
}

#[component]
fn SetupForm(set_screen: WriteSignal<String>) -> impl IntoView {
    let (code, set_code) = signal(String::new());
    let (password, set_password) = signal(String::new());
    let (again, set_again) = signal(String::new());
    let (message, set_message) = signal(String::new());
    let (busy, set_busy) = signal(false);

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if password.get() != again.get() {
            set_message.set("The two passwords do not match.".into());
            return;
        }
        set_busy.set(true);
        set_message.set(String::new());
        let (c, p) = (code.get(), password.get());
        spawn_local(async move {
            let result = show(auth_register(&c, &p).await);
            set_busy.set(false);
            if result.is_empty() {
                set_screen.set("app".into());
            } else {
                set_message.set(result);
            }
        });
    };

    view! {
        <form on:submit=submit>
            <h2>"First-time setup"</h2>
            <p class="muted">
                "Choose the password for LifeUI (12+ characters; a passphrase of 4 or more random words is best). "
                "It is stored only as a salted Argon2id hash. You also need the setup code from the server (SETUP_CODE)."
            </p>
            <label>"Setup code"
                <input type="password" autocomplete="off" required
                    prop:value=code on:input=move |ev| set_code.set(event_target_value(&ev)) />
            </label>
            <label>"New password"
                <input type="password" autocomplete="new-password" required minlength="12"
                    prop:value=password on:input=move |ev| set_password.set(event_target_value(&ev)) />
            </label>
            <label>"Repeat password"
                <input type="password" autocomplete="new-password" required minlength="12"
                    prop:value=again on:input=move |ev| set_again.set(event_target_value(&ev)) />
            </label>
            <button type="submit" class="btn" disabled=move || busy.get()>"Create password"</button>
            <p class="error" role="alert">{move || message.get()}</p>
        </form>
    }
}

#[component]
fn LoginForm(set_screen: WriteSignal<String>) -> impl IntoView {
    let (password, set_password) = signal(String::new());
    let (message, set_message) = signal(String::new());
    let (busy, set_busy) = signal(false);

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        set_busy.set(true);
        set_message.set(String::new());
        let p = password.get();
        spawn_local(async move {
            let result = show(auth_login(&p).await);
            set_busy.set(false);
            if result.is_empty() {
                set_password.set(String::new());
                set_screen.set("app".into());
            } else {
                set_message.set(result);
            }
        });
    };

    view! {
        <form on:submit=submit>
            <h2>"Log in"</h2>
            <label>"Password"
                <input type="password" autocomplete="current-password" required
                    prop:value=password on:input=move |ev| set_password.set(event_target_value(&ev)) />
            </label>
            <button type="submit" class="btn" disabled=move || busy.get()>"Log in"</button>
            <p class="error" role="alert">{move || message.get()}</p>
        </form>
    }
}

/// Phase 1 checks (API/DB ping, passkey experiments), kept under the "Tools" tab.
#[component]
pub fn Diagnostics() -> impl IntoView {
    let (output, set_output) = signal(String::from("Press a button."));

    let run_api = move |path: &'static str| {
        set_output.set(format!("calling {path} ..."));
        spawn_local(async move {
            set_output.set(show(call_api(path).await));
        });
    };

    view! {
        <section>
            <div class="screen-head"><h2>"Tools"</h2></div>
            <p class="muted">"Connection checks and passkey experiments."</p>
            <pre>{environment()}</pre>
            <button class="btn secondary" on:click=move |_| run_api("/api/health")>"Ping Rust API"</button>
            <button class="btn secondary" on:click=move |_| run_api("/api/db_ping")>"Ping database"</button>
            <button class="btn secondary" on:click=move |_| {
                set_output.set("waiting for passkey prompt ...".into());
                spawn_local(async move { set_output.set(show(passkey_create().await)); });
            }>"Create passkey (experiment)"</button>
            <button class="btn secondary" on:click=move |_| {
                set_output.set("waiting for passkey prompt ...".into());
                spawn_local(async move { set_output.set(show(passkey_login().await)); });
            }>"Sign in with passkey (experiment)"</button>
            <pre class="out" aria-live="polite">{move || output.get()}</pre>
        </section>
    }
}

#[component]
fn AppPanel(set_screen: WriteSignal<String>) -> impl IntoView {
    view! { <ui::Home set_screen /> }
}

#[component]
fn App() -> impl IntoView {
    let (screen, set_screen) = signal(String::from("loading"));
    spawn_local(async move {
        set_screen.set(show(auth_state().await));
    });

    view! {
        <header class="appbar">
            <div class="appbar-inner">
                <div class="brand">
                    <img class="mark" src="/logo-mark.png" alt="" width="28" height="28" />
                    <h1>"LifeUI"</h1>
                </div>
                <Show when=move || screen.get() == "app">
                    <button class="icon-btn logout" aria-label="Log out" title="Log out" on:click=move |_| {
                        spawn_local(async move {
                            let _ = auth_logout().await;
                            set_screen.set("login".into());
                        });
                    }><ui::Icon name="logout" /></button>
                </Show>
            </div>
        </header>
        <main>
            {move || {
                let current = screen.get();
                match current.as_str() {
                    "loading" => view! { <p class="muted">"Loading..."</p> }.into_any(),
                    "setup" => view! { <SetupForm set_screen /> }.into_any(),
                    "login" => view! { <LoginForm set_screen /> }.into_any(),
                    "app" => view! { <AppPanel set_screen /> }.into_any(),
                    other => view! {
                        <p class="error">
                            {format!("Cannot reach the server: {}", other.trim_start_matches("error:"))}
                        </p>
                    }
                    .into_any(),
                }
            }}
        </main>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
