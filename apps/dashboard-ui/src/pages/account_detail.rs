use crate::api;
use crate::components::badge::{Badge, BadgeTone};
use crate::components::event_card::EventCard;
use crate::components::stat_card::StatCard;
use crate::components::time::*;
use crate::router::Route;
use dioxus::prelude::*;

#[component]
pub fn AccountDetail(bucket: String, account: String) -> Element {
    let data = use_resource({
        let bucket = bucket.clone();
        let account = account.clone();
        move || {
            let bucket = bucket.clone();
            let account = account.clone();
            async move {
                let (detail, events) = futures_util::join!(
                    api::buckets::get_bucket_detail(&bucket),
                    api::buckets::list_bucket_events(&bucket, "", &account, 1, 100),
                );
                (detail.ok(), events.ok())
            }
        }
    });

    match &*data.read() {
        Some((Some(detail), Some(events))) => {
            let summary = detail.accounts.iter().find(|a| a.account == account);
            let now = js_sys::Date::now() as u64;
            let active_holds: Vec<_> = events
                .events
                .iter()
                .filter(|e| e.hold_amount > 0 && e.hold_expires_at_unix_ms > now)
                .collect();

            rsx! {
                div { class: "grid gap-6 w-full",
                section { class: "flex flex-wrap justify-between items-start gap-4",
                    div { class: "grid gap-1",
                        span { class: "text-accent-100 text-xs uppercase tracking-widest",
                            "Account in "
                            Link { to: Route::BucketDetail { bucket: bucket.clone() }, class: "text-accent-100 hover:text-fg no-underline", "{bucket}" }
                        }
                        h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "{account}" }
                    }
                    Link { to: Route::BucketDetail { bucket: bucket.clone() }, class: "px-3.5 py-2 rounded-full border border-base-700 text-base-400 hover:text-fg transition no-underline text-sm", "Back to bucket" }
                }

                section { class: "grid grid-cols-[repeat(auto-fit,minmax(160px,1fr))] gap-4",
                    StatCard { label: "Balance".to_string(), value: summary.map(|s| format_amount(s.balance)).unwrap_or("0".to_string()) }
                    StatCard { label: "Available".to_string(), value: summary.map(|s| format_amount(s.available_balance)).unwrap_or("0".to_string()) }
                    StatCard { label: "Active holds".to_string(), value: summary.map(|s| format_amount(s.active_hold_total)).unwrap_or("0".to_string()) }
                    StatCard { label: "Events".to_string(), value: summary.map(|s| s.event_count.to_string()).unwrap_or("0".to_string()) }
                }

                // Active holds
                section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-5",
                    div { class: "flex justify-between items-start",
                        h2 { class: "text-[16px] font-normal", "Active holds" }
                        Badge { text: format!("{} holds", active_holds.len()), tone: if active_holds.is_empty() { BadgeTone::Neutral } else { BadgeTone::Warning } }
                    }
                    if active_holds.is_empty() {
                        div { class: "py-7 text-center text-base-500 border border-dashed border-base-800 rounded-[14px]", "No active holds on this account." }
                    } else {
                        div { class: "grid gap-2.5",
                            for event in &active_holds {
                                EventCard { event: (*event).clone(), show_account: false, show_node: true }
                            }
                        }
                    }
                }

                // Recent events
                section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-5",
                    div { class: "flex justify-between items-start",
                        h2 { class: "text-[16px] font-normal", "Recent events" }
                        Badge { text: format!("{} total", events.total), tone: BadgeTone::Neutral }
                    }
                    if events.events.is_empty() {
                        div { class: "py-7 text-center text-base-500 border border-dashed border-base-800 rounded-[14px]", "No events on this account yet." }
                    } else {
                        div { class: "grid gap-2.5",
                            for event in events.events.iter().take(25) {
                                EventCard { event: event.clone(), show_account: false, show_node: true }
                            }
                        }
                    }
                }
                }
            }
        }
        _ => rsx! {
            div { class: "text-base-500 text-center py-12", "Loading…" }
        },
    }
}
