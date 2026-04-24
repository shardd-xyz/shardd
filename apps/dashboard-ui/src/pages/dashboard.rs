use crate::api;
use crate::components::badge::{Badge, BadgeTone};
use crate::components::edge_status::EdgeMeshStatus;
use crate::components::stat_card::StatCard;
use crate::components::time::*;
use crate::router::Route;
use dioxus::prelude::*;

#[component]
pub fn Dashboard() -> Element {
    let data = use_resource(|| async {
        let (dev, keys, buckets, edges) = futures_util::join!(
            api::developer::me(),
            api::developer::list_keys(),
            api::buckets::list_buckets("", 1, 3, "active"),
            api::buckets::list_edges(),
        );
        (dev.ok(), keys.ok(), buckets.ok(), edges.unwrap_or_default())
    });

    match &*data.read() {
        Some((dev, keys, buckets, edges)) => {
            let active_keys = keys
                .as_ref()
                .map(|k| k.iter().filter(|k| k.revoked_at.is_none()).count())
                .unwrap_or(0);
            let last_used = keys.as_ref().and_then(|k| {
                k.iter()
                    .filter_map(|k| k.last_used_at.as_deref())
                    .max()
                    .map(String::from)
            });
            let bucket_data = buckets.as_ref();
            let is_frozen = dev.as_ref().map(|d| d.is_frozen).unwrap_or(false);

            rsx! {
                section { class: "flex flex-wrap justify-between items-start gap-4",
                    div { class: "grid gap-1",
                        span { class: "text-accent-100 text-xs uppercase tracking-widest", "Developer" }
                        h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "Developer home" }
                    }
                    div { class: "flex gap-2.5",
                        Link { to: Route::Keys, class: "bg-[var(--btn-primary-bg)] hover:bg-base-800 text-[var(--btn-primary-text)] font-normal px-4 py-2 rounded-lg transition no-underline text-sm", "Create API key" }
                        Link { to: Route::Buckets, class: "px-3.5 py-2 rounded-full border border-base-700 text-base-400 hover:text-fg transition no-underline text-sm", "Open buckets" }
                    }
                }

                if is_frozen {
                    section { class: "flex flex-wrap items-baseline gap-2 px-4 py-3 rounded-sm border border-accent-200/30 bg-base-900",
                        strong { class: "text-fg", "API access is frozen." }
                        span { class: "text-base-400", "Contact an admin to restore key activity." }
                    }
                }

                section { class: "grid grid-cols-[repeat(auto-fit,minmax(160px,1fr))] gap-4",
                    StatCard { label: "Active keys".to_string(), value: active_keys.to_string() }
                    StatCard {
                        label: "Last key used".to_string(),
                        value: if last_used.is_some() { format_relative_time_str(last_used.as_deref()) } else { "never".to_string() },
                        title: format_date_str(last_used.as_deref()),
                    }
                    StatCard { label: "Buckets".to_string(), value: bucket_data.map(|b| b.total.to_string()).unwrap_or("0".to_string()) }
                }

                if !edges.is_empty() {
                    EdgeMeshStatus { edges: edges.clone() }
                }

                section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-5",
                    h2 { class: "text-[16px] font-mono font-normal text-fg", "Quick actions" }
                    div { class: "grid grid-cols-2 gap-4 max-[640px]:grid-cols-1",
                        Link { to: Route::Keys, class: "rounded-lg border border-base-800 bg-base-900 p-4 grid gap-1 hover:border-base-700 transition no-underline",
                            span { class: "text-accent-100 text-xs uppercase tracking-widest", "Keys" }
                            strong { class: "text-fg", "Create API key" }
                            span { class: "text-base-500 text-sm", "Issue and scope credentials for scripts, workers, or local development." }
                        }
                        Link { to: Route::Buckets, class: "rounded-lg border border-base-800 bg-base-900 p-4 grid gap-1 hover:border-base-700 transition no-underline",
                            span { class: "text-accent-100 text-xs uppercase tracking-widest", "Buckets" }
                            strong { class: "text-fg", "Explore buckets" }
                            span { class: "text-base-500 text-sm", "Inspect balances, review events, and open the bucket you need." }
                        }
                        a {
                            href: "https://shardd.xyz/guide/quickstart",
                            target: "_blank",
                            rel: "noopener",
                            class: "rounded-lg border border-base-800 bg-base-900 p-4 grid gap-1 hover:border-base-700 transition no-underline",
                            span { class: "text-accent-100 text-xs uppercase tracking-widest", "Docs \u{2197}" }
                            strong { class: "text-fg", "Read the quickstart" }
                            span { class: "text-base-500 text-sm", "Copy-pasteable curl/SDK calls for every endpoint. Same docs the landing page links to." }
                        }
                        a {
                            href: "https://github.com/sssemil/shardd/tree/main/apps/cli",
                            target: "_blank",
                            rel: "noopener",
                            class: "rounded-lg border border-base-800 bg-base-900 p-4 grid gap-1 hover:border-base-700 transition no-underline",
                            span { class: "text-accent-100 text-xs uppercase tracking-widest", "CLI \u{2197}" }
                            strong { class: "text-fg", "shardd-cli" }
                            span { class: "text-base-500 text-sm", "Power-user tool for scripting writes and inspecting balances from your terminal." }
                        }
                    }
                }

                section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-5",
                    div { class: "flex justify-between items-start",
                        h2 { class: "text-[16px] font-normal", "Recent bucket activity" }
                        if let Some(b) = bucket_data {
                            Badge { text: format!("{} total", b.total), tone: BadgeTone::Neutral }
                        }
                    }
                    if let Some(b) = bucket_data {
                        if b.buckets.is_empty() {
                            crate::components::empty_state::EmptyState {
                                title: "No activity yet".to_string(),
                                body: "Create a bucket, issue an API key, and write your first event. A worked example is one click away.".to_string(),
                                cta: Some(("Go to buckets".to_string(), Route::Buckets)),
                                external_cta: None,
                            }
                        } else {
                            div { class: "grid gap-2",
                                for bucket in &b.buckets {
                                    Link {
                                        to: Route::BucketDetail { bucket: bucket.bucket.clone() },
                                        class: "flex justify-between items-center px-4 py-3 rounded-lg border border-base-800 bg-base-900 hover:border-base-700 transition no-underline",
                                        strong { class: "text-fg font-mono text-sm", "{bucket.bucket}" }
                                        span { class: "text-base-500 text-sm",
                                            title: "{format_date(bucket.last_event_at_unix_ms)}",
                                            "{format_relative_time(bucket.last_event_at_unix_ms)}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None => rsx! {
            div { class: "text-base-500 text-center py-12", "Loading…" }
        },
    }
}
