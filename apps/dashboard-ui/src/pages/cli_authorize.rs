//! /cli-authorize?session=… — the browser side of the customer CLI's
//! device flow.
//!
//! 1. On mount, verify the user is logged in (redirect to /login if not,
//!    preserving the cli-authorize URL so they come back here).
//! 2. Show "Authorize shardd-cli for {email}?" with an Authorize button.
//! 3. On click, POST /api/auth/cli/authorize { session_id } and render
//!    the returned verification_code in a large monospace pill with a
//!    one-click copy button.
//! 4. Tell the user to paste the code into the waiting CLI.
//!
//! The CLI never sees a session cookie or JWT; only the raw API key
//! that /api/auth/cli/exchange mints when the code is presented.

use crate::api::auth::CliAuthorizeResponse;
use crate::router::Route;
use dioxus::prelude::*;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

#[derive(Clone, PartialEq, Debug)]
enum AuthorizeState {
    Loading,
    NoSession,
    Ready { email: String },
    Submitting,
    Done(CliAuthorizeResponse),
    Error(String),
}

#[component]
pub fn CliAuthorize() -> Element {
    let nav = use_navigator();
    let mut state = use_signal(|| AuthorizeState::Loading);
    let mut copied = use_signal(|| false);

    // Read the session from the search string captured pre-router-mount,
    // mirroring how Magic reads ?token=… (Dioxus drops query params on
    // routes that don't declare them in their #[route] attribute).
    let session_id = use_memo(|| {
        let raw = crate::state::INITIAL_SEARCH
            .get()
            .cloned()
            .unwrap_or_default();
        raw.trim_start_matches('?')
            .split('&')
            .find_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                if k == "session" {
                    Some(
                        urlencoding::decode(v)
                            .map(|s| s.into_owned())
                            .unwrap_or_else(|_| v.to_string()),
                    )
                } else {
                    None
                }
            })
            .unwrap_or_default()
    });

    use_effect(move || {
        let sid = session_id.read().clone();
        if sid.is_empty() {
            state.set(AuthorizeState::NoSession);
            return;
        }
        spawn(async move {
            match crate::api::auth::verify().await {
                Ok(s) => state.set(AuthorizeState::Ready { email: s.email }),
                Err(_) => {
                    // Bounce to login. Preserve ?session= so the user
                    // lands back here after logging in.
                    let target = format!("/login?next=/cli-authorize?session={sid}");
                    if let Some(window) = web_sys::window() {
                        let _ = window.location().set_href(&target);
                    } else {
                        nav.push(Route::Login);
                    }
                }
            }
        });
    });

    let on_authorize = move |_| {
        let sid = session_id.read().clone();
        state.set(AuthorizeState::Submitting);
        spawn(async move {
            match crate::api::auth::cli_authorize(&sid).await {
                Ok(resp) => state.set(AuthorizeState::Done(resp)),
                Err(err) => {
                    let msg = if err.message.is_empty() {
                        format!("authorization failed (HTTP {})", err.status)
                    } else {
                        err.message.clone()
                    };
                    state.set(AuthorizeState::Error(msg));
                }
            }
        });
    };

    rsx! {
        div { class: "min-h-screen flex items-center justify-center px-4 py-12 bg-base-1000",
            div { class: "w-full max-w-[480px] grid gap-6",
                header { class: "grid gap-2",
                    span { class: "text-accent-100 text-xs uppercase tracking-widest font-mono", "shardd cli" }
                    h1 { class: "font-mono text-[28px] text-fg leading-tight m-0", "Authorize CLI" }
                }

                {match &*state.read() {
                    AuthorizeState::Loading => rsx! {
                        p { class: "text-base-500 font-mono text-[13px] m-0", "Loading…" }
                    },
                    AuthorizeState::NoSession => rsx! {
                        div { class: "rounded-lg border border-dashed border-base-700 bg-base-900 p-5 grid gap-3",
                            strong { class: "font-mono text-[14px] text-fg", "No session in URL" }
                            p { class: "text-base-400 font-mono text-[13px] leading-[160%] m-0",
                                "This page expects a "
                                code { class: "text-accent-100", "?session=" }
                                " query parameter. It's normally opened by "
                                code { class: "text-accent-100", "shardd auth login" }
                                "."
                            }
                        }
                    },
                    AuthorizeState::Ready { email } => {
                        let email_owned = email.clone();
                        rsx! {
                            div { class: "rounded-lg border border-base-800 bg-base-900 p-5 grid gap-4",
                                p { class: "text-base-400 font-mono text-[14px] leading-[160%] m-0",
                                    "Issue a developer API key for "
                                    strong { class: "text-fg", "{email_owned}" }
                                    " so the waiting "
                                    code { class: "text-accent-100", "shardd" }
                                    " CLI can act on your behalf."
                                }
                                ul { class: "text-base-500 font-mono text-[12px] leading-[160%] m-0 pl-4 list-disc",
                                    li { "The key has full read+write on every bucket you own." }
                                    li { "Manage or revoke it anytime at " Link { to: Route::Keys, class: "text-accent-100 hover:text-fg no-underline", "/dashboard/keys" } "." }
                                    li { "The key shows once on the next screen — paste it into the waiting CLI." }
                                }
                                button {
                                    r#type: "button",
                                    class: "mt-2 px-4 py-2 rounded border border-accent-100 bg-accent-100 text-base-1000 font-mono text-[13px] uppercase tracking-widest hover:bg-accent-200 hover:border-accent-200 transition-colors duration-150 cursor-pointer",
                                    onclick: on_authorize,
                                    "Authorize"
                                }
                            }
                        }
                    }
                    AuthorizeState::Submitting => rsx! {
                        p { class: "text-base-500 font-mono text-[13px] m-0", "Issuing key…" }
                    },
                    AuthorizeState::Done(resp) => {
                        let code = resp.verification_code.clone();
                        let code_for_copy = code.clone();
                        let client_name = resp.client_name.clone();
                        let hostname = resp.hostname.clone();
                        let copied_now = *copied.read();
                        rsx! {
                            div { class: "rounded-lg border border-accent-100/40 bg-base-900 p-5 grid gap-4",
                                div { class: "grid gap-1",
                                    span { class: "text-accent-100 text-xs uppercase tracking-widest font-mono", "Authorized" }
                                    p { class: "text-base-400 font-mono text-[13px] leading-[160%] m-0",
                                        "Paste this code into the waiting "
                                        code { class: "text-accent-100", "{client_name}" }
                                        " on "
                                        code { class: "text-accent-100", "{hostname}" }
                                        ":"
                                    }
                                }
                                pre { class: "p-4 rounded-lg bg-base-1000 border border-base-800 font-mono text-[24px] tracking-[0.18em] text-fg text-center m-0 select-all",
                                    "{code}"
                                }
                                button {
                                    r#type: "button",
                                    class: "px-3 py-1.5 rounded border border-base-700 bg-transparent font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-400 hover:text-fg hover:border-accent-100 transition-colors duration-150 cursor-pointer w-fit",
                                    onclick: move |_| {
                                        let code = code_for_copy.clone();
                                        spawn(async move {
                                            if write_to_clipboard(&code).await.is_ok() {
                                                copied.set(true);
                                                gloo_timers::future::TimeoutFuture::new(2000).await;
                                                copied.set(false);
                                            }
                                        });
                                    },
                                    if copied_now { "✓ copied" } else { "Copy code" }
                                }
                                p { class: "text-base-600 font-mono text-[12px] leading-[160%] m-0",
                                    "The code expires in 10 minutes and can only be exchanged once. After the CLI consumes it, return to "
                                    Link { to: Route::Keys, class: "text-accent-100 hover:text-fg no-underline", "/dashboard/keys" }
                                    " to manage the new key."
                                }
                            }
                        }
                    }
                    AuthorizeState::Error(msg) => {
                        let m = msg.clone();
                        rsx! {
                            div { class: "rounded-lg border border-[#f87171]/40 bg-base-900 p-5 grid gap-2",
                                strong { class: "font-mono text-[14px] text-fg", "Authorization failed" }
                                p { class: "text-base-400 font-mono text-[13px] leading-[160%] m-0", "{m}" }
                                p { class: "text-base-500 font-mono text-[12px] leading-[160%] m-0",
                                    "If the CLI is still waiting, run "
                                    code { class: "text-accent-100", "shardd auth login" }
                                    " again to start a new session."
                                }
                            }
                        }
                    }
                }}
            }
        }
    }
}

async fn write_to_clipboard(text: &str) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or(JsValue::NULL)?;
    let clipboard = window.navigator().clipboard();
    let promise = clipboard.write_text(text);
    JsFuture::from(promise).await.map(|_| ())
}
