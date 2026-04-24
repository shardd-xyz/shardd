use crate::router::Route;
use crate::state::use_session;
use dioxus::prelude::*;

#[component]
pub fn Login() -> Element {
    let mut email = use_signal(String::new);
    let mut status = use_signal(|| LoginStatus::Idle);
    let mut error = use_signal(|| Option::<String>::None);
    let nav = use_navigator();
    let session = use_session();

    if session.read().is_some() {
        nav.push(Route::Dashboard);
    }

    let on_submit = move |evt: FormEvent| {
        evt.prevent_default();
        let email_val = email.read().clone();
        if email_val.trim().is_empty() {
            error.set(Some("Enter your email address.".to_string()));
            return;
        }
        status.set(LoginStatus::Sending);
        error.set(None);
        spawn(async move {
            match crate::api::auth::request_magic_link(&email_val).await {
                Ok(()) => status.set(LoginStatus::Sent),
                Err(e) => {
                    error.set(Some(e.friendly().1));
                    status.set(LoginStatus::Idle);
                }
            }
        });
    };

    rsx! {
        div { class: "min-h-screen flex flex-col",
            div { class: "flex-1 flex items-center justify-center px-4",
                div { class: "w-full max-w-[400px] grid gap-6",
                    a {
                        href: "https://shardd.xyz",
                        class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500 hover:text-fg transition-colors duration-150 no-underline w-fit",
                        "\u{2190} Back to shardd.xyz"
                    }
                    div { class: "grid gap-2",
                    div { class: "flex items-center gap-3 mb-4",
                        svg { width: "28", height: "28", view_box: "0 0 128 128", fill: "none",
                            rect { width: "128", height: "128", rx: "24", fill: "#12202C" }
                            circle { cx: "34", cy: "34", r: "10", fill: "#0F8B8D" }
                            circle { cx: "94", cy: "34", r: "10", fill: "#E85D04" }
                            circle { cx: "64", cy: "94", r: "10", fill: "#F4D35E" }
                            path { d: "M34 34L94 34L64 94L34 34Z", stroke: "#F7F3EC", stroke_width: "8", stroke_linejoin: "round" }
                        }
                        span { class: "font-mono text-[14px] text-fg", "shardd" }
                    }
                    span { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-accent-100", "Authentication" }
                    h1 { class: "text-[32px] font-mono font-normal leading-[100%] tracking-[-0.04em] text-fg", "Sign in" }
                }

                match *status.read() {
                    LoginStatus::Sent => rsx! {
                        div { class: "rounded-lg border border-dashed border-accent-100 bg-base-900 px-4 py-3.5",
                            strong { class: "text-fg font-mono text-[14px] block", "Check your inbox" }
                            p { class: "text-base-400 font-mono text-[14px] mt-1 leading-[140%]", "We sent a sign-in link to {email}." }
                        }
                    },
                    _ => rsx! {
                        form {
                            class: "grid gap-4",
                            onsubmit: on_submit,
                            div { class: "grid gap-1.5",
                                label { r#for: "email", class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-400", "Email" }
                                input {
                                    id: "email",
                                    r#type: "email",
                                    required: true,
                                    placeholder: "you@company.com",
                                    value: "{email}",
                                    oninput: move |evt| email.set(evt.value()),
                                }
                            }
                            if let Some(err) = error.read().as_ref() {
                                p { class: "text-[#f87171] font-mono text-[14px]", "{err}" }
                            }
                            button {
                                r#type: "submit",
                                class: "group relative w-full h-[40px] px-6 font-mono text-[14px] uppercase tracking-[-0.0175rem] bg-[var(--btn-primary-bg)] text-[var(--btn-primary-text)] border border-base-600 rounded-sm overflow-hidden transition-colors duration-150 hover:opacity-80",
                                disabled: matches!(*status.read(), LoginStatus::Sending),
                                div { class: "pointer-events-none absolute inset-0 opacity-0 group-hover:opacity-100 transition-opacity duration-100",
                                    div { class: "btn-stripe-pattern absolute inset-0" }
                                }
                                span { class: "relative z-10",
                                    match *status.read() {
                                        LoginStatus::Sending => "Sending…",
                                        _ => "Send magic link",
                                    }
                                }
                            }
                        }

                        div { class: "flex items-center gap-3 my-2",
                            div { class: "flex-1 border-t border-base-800" }
                            span { class: "font-mono text-[11px] uppercase tracking-[-0.01rem] text-base-600", "or" }
                            div { class: "flex-1 border-t border-base-800" }
                        }

                        a {
                            href: "/api/auth/google",
                            class: "group relative inline-flex w-full items-center justify-center h-[40px] px-6 font-mono text-[14px] uppercase tracking-[-0.0175rem] bg-base-900 text-fg border border-base-600 rounded-sm overflow-hidden transition-colors duration-150 hover:opacity-80 no-underline",
                            div { class: "pointer-events-none absolute inset-0 opacity-0 group-hover:opacity-100 transition-opacity duration-100",
                                div { class: "btn-stripe-pattern absolute inset-0" }
                            }
                            span { class: "relative z-10 flex items-center gap-2",
                                svg {
                                    width: "18",
                                    height: "18",
                                    view_box: "0 0 24 24",
                                    fill: "none",
                                    path {
                                        d: "M22.56 12.25c0-.78-.07-1.53-.2-2.25H12v4.26h5.92a5.06 5.06 0 0 1-2.2 3.32v2.77h3.57c2.08-1.92 3.28-4.74 3.28-8.1z",
                                        fill: "#4285F4",
                                    }
                                    path {
                                        d: "M12 23c2.97 0 5.46-.98 7.28-2.66l-3.57-2.77c-.98.66-2.23 1.06-3.71 1.06-2.86 0-5.29-1.93-6.16-4.53H2.18v2.84C3.99 20.53 7.7 23 12 23z",
                                        fill: "#34A853",
                                    }
                                    path {
                                        d: "M5.84 14.09c-.22-.66-.35-1.36-.35-2.09s.13-1.43.35-2.09V7.07H2.18C1.43 8.55 1 10.22 1 12s.43 3.45 1.18 4.93l2.85-2.22.81-.62z",
                                        fill: "#FBBC05",
                                    }
                                    path {
                                        d: "M12 5.38c1.62 0 3.06.56 4.21 1.64l3.15-3.15C17.45 2.09 14.97 1 12 1 7.7 1 3.99 3.47 2.18 7.07l3.66 2.84c.87-2.6 3.3-4.53 6.16-4.53z",
                                        fill: "#EA4335",
                                    }
                                }
                                "Sign in with Google"
                            }
                        }
                    },
                    }
                }
            }
            footer { class: "border-t border-base-800",
                div { class: "mx-auto max-w-[1180px] px-4 lg:px-9 py-6 flex flex-wrap items-center gap-x-6 gap-y-2 font-mono text-[12px] text-base-600",
                    span { "© 2026 TQDM Inc." }
                    a { href: "/tos", class: "text-base-500 hover:text-fg transition-colors duration-150 no-underline", "Terms" }
                    a { href: "/privacy", class: "text-base-500 hover:text-fg transition-colors duration-150 no-underline", "Privacy" }
                    span { class: "flex-1" }
                    span { class: "text-base-700", "shardd control plane" }
                }
            }
        }
    }
}

#[derive(Clone, PartialEq)]
enum LoginStatus {
    Idle,
    Sending,
    Sent,
}

#[component]
pub fn Magic() -> Element {
    let _session_signal = use_session();
    let nav = use_navigator();

    use_effect(move || {
        spawn(async move {
            // Dioxus router normalizes the URL on mount and drops the
            // `?token=...` query on routes that don't declare it. Read the
            // search string that main.rs captured BEFORE the router mounted.
            let search = crate::state::INITIAL_SEARCH
                .get()
                .cloned()
                .unwrap_or_default();
            let token = search
                .trim_start_matches('?')
                .split('&')
                .find_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    if k == "token" {
                        Some(v.to_string())
                    } else {
                        None
                    }
                })
                .map(|raw| {
                    urlencoding::decode(&raw)
                        .map(|s| s.into_owned())
                        .unwrap_or(raw)
                })
                .unwrap_or_default();

            if token.is_empty() {
                nav.push(Route::Login);
                return;
            }

            match crate::api::auth::consume_magic_link(&token).await {
                Ok(()) => {
                    let window = web_sys::window().unwrap();
                    let _ = window.location().set_href("/dashboard");
                }
                Err(_) => {
                    nav.push(Route::Login);
                }
            }
        });
    });

    rsx! {
        div { class: "min-h-screen flex items-center justify-center text-base-500 font-mono text-[14px]",
            "Authenticating…"
        }
    }
}
