use crate::api;
use crate::router::Route;
use dioxus::prelude::*;

/// Shell-registered ⌘K / Ctrl-K command palette. Indexes routes + live
/// bucket names + live key names; fuzzy-filters as the user types; arrow
/// keys + enter to navigate. Native <dialog> element gives us Esc-to-close
/// and click-outside-to-close for free.
#[component]
pub fn CommandPalette(open: Signal<bool>) -> Element {
    let mut query = use_signal(String::new);
    let mut highlight = use_signal(|| 0usize);
    let nav = use_navigator();

    // Live data: buckets + keys. Fetched once when the palette first opens
    // and cached for the lifetime of the component (Signal-backed Resource
    // auto-refreshes on reopen if desired).
    let buckets =
        use_resource(|| async { api::buckets::list_buckets("", 1, 100, "active").await.ok() });
    let keys = use_resource(|| async { api::developer::list_keys().await.ok() });

    if !*open.read() {
        return rsx! {};
    }

    // Build the current item list by filtering against the query.
    let q = query.read().to_lowercase();
    let mut items: Vec<PaletteItem> = Vec::new();

    // Static routes first — they're always available.
    let routes: &[(&str, Route, &str)] = &[
        ("Home", Route::Dashboard, "Dashboard"),
        ("Keys", Route::Keys, "API keys"),
        ("Buckets", Route::Buckets, "Bucket explorer"),
        ("Events", Route::Events, "Event log"),
        ("Billing", Route::Billing, "Billing"),
        ("Profile", Route::Profile, "Profile"),
    ];
    for (label, route, sub) in routes {
        if q.is_empty() || label.to_lowercase().contains(&q) || sub.to_lowercase().contains(&q) {
            items.push(PaletteItem {
                label: label.to_string(),
                sub: sub.to_string(),
                kind: "Page",
                action: PaletteAction::Route(route.clone()),
            });
        }
    }

    // Docs entry — external.
    if q.is_empty() || "docs".contains(&q) || "quickstart".contains(&q) {
        items.push(PaletteItem {
            label: "Docs".to_string(),
            sub: "shardd.xyz/guide/quickstart".to_string(),
            kind: "Link",
            action: PaletteAction::External("https://shardd.xyz/guide/quickstart".to_string()),
        });
    }

    // Buckets.
    if let Some(Some(b)) = buckets.read().as_ref() {
        for bucket in b.buckets.iter() {
            if q.is_empty() || bucket.bucket.to_lowercase().contains(&q) {
                items.push(PaletteItem {
                    label: bucket.bucket.clone(),
                    sub: format!("{} events", bucket.event_count.unwrap_or(0)),
                    kind: "Bucket",
                    action: PaletteAction::Route(Route::BucketDetail {
                        bucket: bucket.bucket.clone(),
                    }),
                });
            }
        }
    }

    // Keys.
    if let Some(Some(k)) = keys.read().as_ref() {
        for key in k.iter() {
            if q.is_empty() || key.name.to_lowercase().contains(&q) {
                items.push(PaletteItem {
                    label: key.name.clone(),
                    sub: format!("key \u{2022} {}", &key.id[..8.min(key.id.len())]),
                    kind: "Key",
                    action: PaletteAction::Route(Route::Keys),
                });
            }
        }
    }

    let max_idx = items.len().saturating_sub(1);
    let current = *highlight.read();
    let current = current.min(max_idx);

    rsx! {
        div {
            class: "fixed inset-0 z-[100] bg-black/60 flex items-start justify-center pt-[15vh] px-4",
            onclick: move |_| open.set(false),
            div {
                class: "w-full max-w-[560px] rounded-lg border border-base-700 bg-base-900 overflow-hidden shadow-2xl",
                // Stop outside click from closing when the click is inside.
                onclick: move |e| e.stop_propagation(),
                input {
                    r#type: "text",
                    placeholder: "Jump to a bucket, key, or page\u{2026}",
                    autofocus: true,
                    value: "{query}",
                    // Dioxus's `autofocus: true` doesn't reliably fire when
                    // the palette is conditionally rendered (toggling
                    // `open` remounts the subtree but the browser only
                    // auto-focuses on initial page load). Grab the element
                    // as it mounts and call .focus() directly.
                    onmounted: move |ctx| {
                        spawn(async move {
                            let _ = ctx.data().set_focus(true).await;
                        });
                    },
                    oninput: move |e| {
                        query.set(e.value());
                        highlight.set(0);
                    },
                    onkeydown: move |e| {
                        match e.key() {
                            Key::Escape => open.set(false),
                            Key::ArrowDown => {
                                let cur = *highlight.read();
                                if cur < max_idx {
                                    highlight.set(cur + 1);
                                }
                                e.prevent_default();
                            }
                            Key::ArrowUp => {
                                let cur = *highlight.read();
                                if cur > 0 {
                                    highlight.set(cur - 1);
                                }
                                e.prevent_default();
                            }
                            Key::Enter => {
                                if current <= max_idx && !items.is_empty() {
                                    match items[current].action.clone() {
                                        PaletteAction::Route(r) => {
                                            nav.push(r);
                                            open.set(false);
                                        }
                                        PaletteAction::External(href) => {
                                            if let Some(w) = web_sys::window() {
                                                let _ = w.open_with_url_and_target(&href, "_blank");
                                            }
                                            open.set(false);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    },
                    class: "w-full px-4 py-3 bg-transparent border-0 border-b border-base-800 outline-none text-fg font-mono text-[15px] placeholder:text-base-500",
                }
                div { class: "max-h-[400px] overflow-y-auto",
                    if items.is_empty() {
                        div { class: "px-4 py-6 text-center text-base-500 font-mono text-[13px]",
                            "No matches."
                        }
                    }
                    for (idx, item) in items.iter().enumerate() {
                        {
                            let is_active = idx == current;
                            let cls = if is_active {
                                "flex items-center gap-3 px-4 py-2.5 bg-base-1000 cursor-pointer"
                            } else {
                                "flex items-center gap-3 px-4 py-2.5 hover:bg-base-1000/60 cursor-pointer"
                            };
                            let action = item.action.clone();
                            rsx! {
                                button {
                                    key: "{idx}",
                                    class: "{cls} w-full text-left border-0 bg-transparent",
                                    onclick: move |_| {
                                        match action.clone() {
                                            PaletteAction::Route(r) => {
                                                nav.push(r);
                                                open.set(false);
                                            }
                                            PaletteAction::External(href) => {
                                                if let Some(w) = web_sys::window() {
                                                    let _ = w.open_with_url_and_target(&href, "_blank");
                                                }
                                                open.set(false);
                                            }
                                        }
                                    },
                                    span { class: "font-mono text-[11px] uppercase tracking-[-0.015rem] text-base-500 w-14 shrink-0", "{item.kind}" }
                                    span { class: "font-mono text-[14px] text-fg flex-1", "{item.label}" }
                                    span { class: "font-mono text-[12px] text-base-500 truncate max-w-[180px]", "{item.sub}" }
                                }
                            }
                        }
                    }
                }
                div { class: "flex items-center gap-4 px-4 py-2 border-t border-base-800 bg-base-1000 font-mono text-[11px] text-base-500",
                    span { "\u{2191}\u{2193} navigate" }
                    span { "\u{21b5} open" }
                    span { "esc close" }
                }
            }
        }
    }
}

#[derive(Clone)]
struct PaletteItem {
    label: String,
    sub: String,
    kind: &'static str,
    action: PaletteAction,
}

#[derive(Clone)]
enum PaletteAction {
    Route(Route),
    External(String),
}
