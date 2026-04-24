use crate::components::command_palette::CommandPalette;
use crate::router::Route;
use crate::state::{load_flash_key_from_session, use_flash_key, use_notice, use_session};
use crate::types::NoticeTone;
use dioxus::prelude::*;
use wasm_bindgen::{JsCast, closure::Closure};

#[component]
pub fn ShellLayout() -> Element {
    let session = use_session();
    let nav = use_navigator();
    let current_route: Route = use_route();

    use_effect(move || {
        spawn(async move {
            match crate::api::auth::verify().await {
                Ok(s) => {
                    let mut session = use_context::<Signal<Option<crate::types::Session>>>();
                    session.set(Some(s));
                }
                Err(_) => {
                    nav.push(Route::Login);
                }
            }
        });
    });

    // Hydrate any pending API-key flash from sessionStorage so a dev who
    // reloaded or navigated away can still copy the one-shot raw key.
    let mut flash = use_flash_key();
    use_effect(move || {
        if flash.read().is_none()
            && let Some(fk) = load_flash_key_from_session()
        {
            flash.set(Some(fk));
        }
    });

    // ⌘K / Ctrl-K opens the command palette from any page. Registered once
    // at shell mount; the listener leaks a closure for the lifetime of the
    // SPA (there is no unmount — the shell IS the whole app).
    let mut palette_open = use_signal(|| false);
    use_effect(move || {
        let Some(window) = web_sys::window() else {
            return;
        };
        let handler = Closure::wrap(Box::new(move |event: web_sys::KeyboardEvent| {
            if event.key() == "k" && (event.meta_key() || event.ctrl_key()) {
                event.prevent_default();
                let current = *palette_open.peek();
                palette_open.set(!current);
            }
        }) as Box<dyn FnMut(_)>);
        let _ =
            window.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
        handler.forget();
    });

    let session_val = session.read();
    if session_val.is_none() {
        return rsx! {
            div { class: "flex items-center justify-center min-h-screen text-base-500 font-mono text-[14px]",
                "Loading…"
            }
        };
    }
    let s = session_val.as_ref().unwrap();

    let is_admin_route = matches!(
        current_route,
        Route::AdminOverview
            | Route::AdminUsers
            | Route::AdminUser { .. }
            | Route::AdminAudit
            | Route::AdminEvents
            | Route::AdminMesh
    );

    rsx! {
        div { class: "min-h-screen flex flex-col",
            header { class: "border-b border-base-800",
                div { class: "mx-auto max-w-[1180px] flex flex-wrap items-center gap-3.5 px-4 lg:px-9 py-3.5 min-h-[56px]",
                    Link {
                        to: Route::Dashboard,
                        class: "flex items-center gap-3 no-underline",
                        svg { width: "28", height: "28", view_box: "0 0 128 128", fill: "none",
                            rect { width: "128", height: "128", rx: "24", fill: "#12202C" }
                            circle { cx: "34", cy: "34", r: "10", fill: "#0F8B8D" }
                            circle { cx: "94", cy: "34", r: "10", fill: "#E85D04" }
                            circle { cx: "64", cy: "94", r: "10", fill: "#F4D35E" }
                            path { d: "M34 34L94 34L64 94L34 34Z", stroke: "#F7F3EC", stroke_width: "8", stroke_linejoin: "round" }
                        }
                        span { class: "font-mono text-[14px] text-fg tracking-[-0.015rem]", "shardd" }
                    }

                    if s.is_admin {
                        {
                            let dev_cls = if !is_admin_route {
                                "px-3 py-1.5 rounded-sm font-mono text-[12px] uppercase tracking-[-0.015rem] text-fg border border-base-800 bg-base-1000 no-underline"
                            } else {
                                "px-3 py-1.5 rounded-sm font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-400 border border-transparent hover:text-accent-100 transition-colors duration-150 no-underline"
                            };
                            let admin_cls = if is_admin_route {
                                "px-3 py-1.5 rounded-sm font-mono text-[12px] uppercase tracking-[-0.015rem] text-fg border border-base-800 bg-base-1000 no-underline"
                            } else {
                                "px-3 py-1.5 rounded-sm font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-400 border border-transparent hover:text-accent-100 transition-colors duration-150 no-underline"
                            };
                            rsx! {
                                nav { class: "flex gap-1",
                                    Link { to: Route::Dashboard, class: "{dev_cls}", "Developer" }
                                    Link { to: Route::AdminOverview, class: "{admin_cls}", "Admin" }
                                }
                            }
                        }
                    }

                    // Command palette trigger: visible hint so users discover
                    // the ⌘K shortcut. Clicking opens the palette the same way.
                    button {
                        r#type: "button",
                        class: "ml-auto hidden min-[720px]:flex items-center gap-2 px-2.5 py-1.5 rounded border border-base-700 text-base-500 hover:text-fg transition-colors duration-150 font-mono text-[12px] cursor-pointer bg-transparent",
                        title: "Open command palette",
                        onclick: move |_| {
                            let current = *palette_open.peek();
                            palette_open.set(!current);
                        },
                        span { "Search" }
                        span { class: "px-1.5 py-0.5 rounded border border-base-800 text-base-500 text-[10px] tracking-wider",
                            if is_apple_platform() { "\u{2318}K" } else { "Ctrl K" }
                        }
                    }

                    details { class: "relative group min-[720px]:ml-2 max-[719px]:ml-auto",
                        summary { class: "relative font-mono text-[13px] text-fg tracking-[-0.0175rem] cursor-pointer list-none px-3 py-1.5 rounded border border-base-700 overflow-hidden max-w-[180px]",
                            div { class: "pointer-events-none absolute inset-0 opacity-0 group-hover:opacity-100 transition-opacity duration-100",
                                div {
                                    class: "btn-stripe-pattern absolute inset-0",
                                    style: "--lines-color: var(--color-base-600)",
                                }
                            }
                            span { class: "relative z-10 block truncate", "{s.email}" }
                        }
                        div {
                            class: "absolute right-0 top-full mt-2 w-[240px] rounded-lg border border-dashed border-base-700 bg-[var(--background)] p-3 grid gap-2 z-50",
                            onclick: move |_| {
                                if let Some(w) = web_sys::window()
                                    && let Some(doc) = w.document()
                                    && let Some(el) = doc.query_selector("details[open].group").ok().flatten()
                                {
                                    el.remove_attribute("open").ok();
                                }
                            },
                            span { class: "font-mono text-[14px] text-fg", "{s.email}" }
                            span { class: "font-mono text-[11px] text-base-500 uppercase tracking-[-0.01rem]",
                                if s.is_admin { "Admin account" } else { "Developer account" }
                            }
                            div { class: "border-t border-base-800 my-1" }
                            Link { to: Route::Profile, class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-400 hover:text-accent-100 transition-colors duration-150 no-underline",
                                "Profile"
                            }
                            button {
                                class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-400 hover:text-accent-100 transition-colors duration-150 bg-transparent border-0 text-left p-0",
                                onclick: move |_| {
                                    spawn(async move {
                                        let _ = crate::api::auth::logout().await;
                                        let window = web_sys::window().unwrap();
                                        let _ = window.location().set_href("/login");
                                    });
                                },
                                "Sign out"
                            }
                        }
                    }
                }

                if !matches!(current_route, Route::Profile) {
                    div { class: "mx-auto max-w-[1180px] px-4 lg:px-9 pb-2.5",
                        if is_admin_route {
                            nav { class: "flex gap-1 overflow-x-auto",
                                SubnavLink { to: Route::AdminOverview, label: "Overview", active: matches!(current_route, Route::AdminOverview) }
                                SubnavLink { to: Route::AdminUsers, label: "Users", active: matches!(current_route, Route::AdminUsers | Route::AdminUser { .. }) }
                                SubnavLink { to: Route::AdminEvents, label: "Events", active: matches!(current_route, Route::AdminEvents) }
                                SubnavLink { to: Route::AdminAudit, label: "Audit", active: matches!(current_route, Route::AdminAudit) }
                                SubnavLink { to: Route::AdminMesh, label: "Mesh", active: matches!(current_route, Route::AdminMesh) }
                            }
                        } else {
                            nav { class: "flex gap-1 overflow-x-auto",
                                SubnavLink { to: Route::Dashboard, label: "Home", active: matches!(current_route, Route::Dashboard) }
                                SubnavLink { to: Route::Keys, label: "Keys", active: matches!(current_route, Route::Keys) }
                                SubnavLink { to: Route::Buckets, label: "Buckets", active: matches!(current_route, Route::Buckets | Route::BucketDetail { .. } | Route::AccountDetail { .. }) }
                                SubnavLink { to: Route::Events, label: "Events", active: matches!(current_route, Route::Events) }
                                SubnavLink { to: Route::Billing, label: "Billing", active: matches!(current_route, Route::Billing) }
                                a {
                                    href: "https://shardd.xyz/guide/quickstart",
                                    target: "_blank",
                                    rel: "noopener",
                                    class: "px-2 py-0.5 rounded font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500 border border-transparent hover:text-fg transition-colors duration-150 whitespace-nowrap no-underline",
                                    "Docs \u{2197}"
                                }
                            }
                        }
                    }
                }
            }

            NoticeBanner {}
            if !is_admin_route {
                CreditAlert {}
            }

            main { class: "mx-auto max-w-[1180px] w-full px-4 lg:px-9 py-6 grid gap-6 [&>*]:w-full [&>*>*]:w-full",
                Outlet::<Route> {}
            }

            Footer {}
        }
        CommandPalette { open: palette_open }
    }
}

/// Persistent amber banner when the user's credit balance drops under 10%
/// of their monthly allowance. Hoisted out of /billing so devs see it from
/// every page rather than only when they happen to check billing.
#[component]
fn CreditAlert() -> Element {
    let status = use_resource(|| async { crate::api::billing::status().await.ok() });
    let status_read = status.read();
    let Some(Some(s)) = status_read.as_ref() else {
        return rsx! {};
    };
    if s.monthly_credits <= 0 {
        return rsx! {};
    }
    let pct = (s.credit_balance as f64 / s.monthly_credits as f64) * 100.0;
    if pct >= 10.0 {
        return rsx! {};
    }
    let tone_border = if pct < 5.0 {
        "border-[#f87171]/40"
    } else {
        "border-accent-200"
    };
    rsx! {
        section { class: "mx-auto max-w-[1180px] w-full px-4 lg:px-9 pt-4",
            div { class: "w-full rounded-lg px-4 py-3 border border-dashed {tone_border} bg-base-900 flex flex-wrap items-center gap-3",
                strong { class: "text-fg font-mono text-[14px]",
                    "Credits running low"
                }
                span { class: "text-base-400 font-mono text-[13px]",
                    "{s.credit_balance} / {s.monthly_credits} remaining. New writes will fail once you hit zero."
                }
                Link {
                    to: Route::Billing,
                    class: "ml-auto font-mono text-[12px] uppercase tracking-[-0.015rem] text-accent-100 hover:text-fg transition-colors duration-150 no-underline",
                    "Top up \u{2192}"
                }
            }
        }
    }
}

#[component]
fn Footer() -> Element {
    rsx! {
        footer { class: "border-t border-base-800 mt-auto",
            div { class: "mx-auto max-w-[1180px] px-4 lg:px-9 py-6 flex flex-wrap items-center gap-x-6 gap-y-2 font-mono text-[12px] text-base-600",
                span { "© 2026 TQDM Inc." }
                Link { to: Route::Tos, class: "text-base-500 hover:text-fg transition-colors duration-150 no-underline", "Terms" }
                Link { to: Route::Privacy, class: "text-base-500 hover:text-fg transition-colors duration-150 no-underline", "Privacy" }
                span { class: "flex-1" }
                span { class: "text-base-700", "shardd control plane" }
            }
        }
    }
}

#[component]
fn NoticeBanner() -> Element {
    let mut notice = use_notice();
    let mut remaining = use_signal(|| 5u32);

    use_effect(move || {
        let current_gen = notice.read().as_ref().map(|n| n.generation);
        remaining.set(5);
        if let Some(g) = current_gen {
            spawn(async move {
                for i in (0..5).rev() {
                    gloo_timers::future::TimeoutFuture::new(1000).await;
                    if notice.read().as_ref().map(|n| n.generation) != Some(g) {
                        return;
                    }
                    remaining.set(i);
                }
                if notice.read().as_ref().map(|n| n.generation) == Some(g) {
                    notice.set(None);
                }
            });
        }
    });

    let n_read = notice.read();
    let Some(n) = n_read.as_ref() else {
        return rsx! {};
    };
    let border = match n.tone {
        NoticeTone::Success => "border-accent-100",
        NoticeTone::Warning => "border-accent-200",
        NoticeTone::Danger => "border-[#f87171]/30",
        NoticeTone::Info => "border-base-700",
    };
    let title = n.title.clone();
    let message = n.message.clone();
    let rem = *remaining.read();
    drop(n_read);

    rsx! {
        section { class: "mx-auto max-w-[1180px] w-full px-4 lg:px-9 pt-4",
            div { class: "w-full rounded-lg px-4 py-3 border border-dashed {border} bg-base-900 grid gap-1",
                strong { class: "text-fg font-mono text-[14px]", "{title}" }
                p { class: "text-base-400 font-mono text-[14px] leading-[140%] m-0", "{message}" }
                div { class: "flex items-center gap-3",
                    button {
                        class: "text-base-500 hover:text-fg font-mono text-[12px] uppercase tracking-[-0.015rem] bg-transparent border-0 w-fit transition-colors duration-150",
                        onclick: move |_| notice.set(None),
                        "Dismiss"
                    }
                    span { class: "text-base-600 font-mono text-[11px]", "Closing in {rem}s" }
                }
            }
        }
    }
}

/// Tell Mac from PC so the hint button shows the right glyph.
fn is_apple_platform() -> bool {
    web_sys::window()
        .and_then(|w| w.navigator().user_agent().ok())
        .map(|ua| {
            let ua = ua.to_lowercase();
            ua.contains("mac") || ua.contains("iphone") || ua.contains("ipad")
        })
        .unwrap_or(true)
}

#[component]
fn SubnavLink(to: Route, label: &'static str, active: bool) -> Element {
    let cls = if active {
        "px-2 py-0.5 rounded font-mono text-[12px] uppercase tracking-[-0.015rem] text-fg border border-base-800 bg-base-1000 whitespace-nowrap no-underline"
    } else {
        "px-2 py-0.5 rounded font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500 border border-transparent hover:text-fg transition-colors duration-150 whitespace-nowrap no-underline"
    };
    rsx! {
        Link { to: to, class: "{cls}", "{label}" }
    }
}
