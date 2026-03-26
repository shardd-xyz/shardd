use dioxus::prelude::*;
use shardd_types::Event;

use crate::api;
use crate::components::sync_chart::SyncTrace;

#[component]
pub fn EventSection(
    node_urls: Vec<String>,
    events: Vec<Event>,
    sync_trace: Signal<SyncTrace>,
) -> Element {
    let mut amount_input = use_signal(|| String::new());
    let mut note_input = use_signal(|| String::new());
    let mut target_idx = use_signal(|| 0usize);
    let mut status_msg = use_signal(|| String::new());
    let mut status_is_error = use_signal(|| false);
    let urls = node_urls.clone();

    rsx! {
        section { class: "mb-8",
            h2 { class: "text-sm font-semibold uppercase tracking-widest text-slate-500 mb-4", "Create Event" }
            div { class: "bg-slate-900/80 border border-slate-800 rounded-xl p-5 mb-6",
                div { class: "flex flex-wrap items-end gap-3",
                    div { class: "min-w-[180px]",
                        label { class: "block text-[11px] uppercase tracking-wider text-slate-500 mb-1.5", "Target node" }
                        select {
                            class: "w-full bg-slate-800 border border-slate-700 text-slate-300 text-sm rounded-lg px-3 py-2 focus:outline-none focus:ring-2 focus:ring-violet-500/50",
                            onchange: move |e| {
                                if let Ok(i) = e.value().parse::<usize>() {
                                    target_idx.set(i);
                                }
                            },
                            for (i, url) in urls.iter().enumerate() {
                                option { value: "{i}", "{url}" }
                            }
                        }
                    }
                    div {
                        label { class: "block text-[11px] uppercase tracking-wider text-slate-500 mb-1.5", "Amount" }
                        input {
                            r#type: "number",
                            class: "w-28 bg-slate-800 border border-slate-700 text-slate-300 text-sm rounded-lg px-3 py-2 focus:outline-none focus:ring-2 focus:ring-violet-500/50",
                            placeholder: "0",
                            value: "{amount_input}",
                            oninput: move |e| amount_input.set(e.value()),
                        }
                    }
                    div { class: "flex-1 min-w-[150px]",
                        label { class: "block text-[11px] uppercase tracking-wider text-slate-500 mb-1.5", "Note" }
                        input {
                            r#type: "text",
                            class: "w-full bg-slate-800 border border-slate-700 text-slate-300 text-sm rounded-lg px-3 py-2 focus:outline-none focus:ring-2 focus:ring-violet-500/50",
                            placeholder: "Optional",
                            value: "{note_input}",
                            oninput: move |e| note_input.set(e.value()),
                        }
                    }
                    button {
                        class: "bg-violet-600 hover:bg-violet-500 text-white text-sm font-medium px-6 py-2 rounded-lg transition-colors whitespace-nowrap",
                        onclick: move |_| {
                            let urls = node_urls.clone();
                            let idx = *target_idx.read();
                            let amount_str = amount_input.read().clone();
                            let note_str = note_input.read().clone();
                            let mut sync_trace = sync_trace;
                            spawn(async move {
                                let Some(url) = urls.get(idx) else { return };
                                let Ok(amount) = amount_str.parse::<i64>() else {
                                    status_msg.set("Invalid amount".into());
                                    status_is_error.set(true);
                                    return;
                                };
                                let note = if note_str.is_empty() { None } else { Some(note_str) };
                                match api::create_event(url, amount, note).await {
                                    Ok(resp) => {
                                        let target_count = resp.event_count;
                                        let short_id = &resp.event.event_id[..8];
                                        status_msg.set(format!("Event {short_id}... created — tracking sync..."));
                                        status_is_error.set(false);
                                        amount_input.set(String::new());
                                        note_input.set(String::new());

                                        // Start sync tracking
                                        let total = urls.len();
                                        sync_trace.set(SyncTrace {
                                            total_nodes: total,
                                            target_event_count: target_count,
                                            data_points: vec![(0.0, 1)], // creator node has it
                                            complete: false,
                                        });

                                        let start = js_sys::Date::now();
                                        let mut last_synced = 1;

                                        for _ in 0..200 { // max ~20 seconds
                                            gloo_timers::future::sleep(std::time::Duration::from_millis(100)).await;
                                            let elapsed = js_sys::Date::now() - start;

                                            let states = api::fetch_all_states(&urls).await;
                                            let synced = states
                                                .iter()
                                                .filter(|(_, s)| s.event_count >= target_count)
                                                .count();

                                            if synced != last_synced || synced == total {
                                                let mut trace = sync_trace.read().clone();
                                                trace.data_points.push((elapsed, synced));
                                                if synced >= total {
                                                    trace.complete = true;
                                                }
                                                sync_trace.set(trace);
                                                last_synced = synced;
                                            }

                                            if synced >= total {
                                                status_msg.set(format!(
                                                    "Event {short_id}... synced to all {total} nodes in {elapsed:.0}ms"
                                                ));
                                                break;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        status_msg.set(format!("Error: {e}"));
                                        status_is_error.set(true);
                                    }
                                }
                            });
                        },
                        "Create"
                    }
                }
                if !status_msg.read().is_empty() {
                    div {
                        class: "mt-3 text-xs px-3 py-2 rounded-lg",
                        class: if *status_is_error.read() { "bg-rose-500/10 text-rose-400" } else { "bg-emerald-500/10 text-emerald-400" },
                        "{status_msg}"
                    }
                }
            }

            h2 { class: "text-sm font-semibold uppercase tracking-widest text-slate-500 mb-4", "Event Log" }
            if events.is_empty() {
                div { class: "bg-slate-900/40 border border-slate-800 rounded-xl p-8 text-center",
                    p { class: "text-slate-600 text-sm", "No events yet" }
                }
            } else {
                div { class: "bg-slate-900/80 border border-slate-800 rounded-xl overflow-hidden",
                    div { class: "overflow-x-auto",
                        table { class: "w-full text-sm",
                            thead {
                                tr { class: "border-b border-slate-800",
                                    th { class: "text-left text-[11px] uppercase tracking-wider text-slate-500 font-medium px-4 py-3", "Origin" }
                                    th { class: "text-left text-[11px] uppercase tracking-wider text-slate-500 font-medium px-4 py-3", "Seq" }
                                    th { class: "text-right text-[11px] uppercase tracking-wider text-slate-500 font-medium px-4 py-3", "Amount" }
                                    th { class: "text-left text-[11px] uppercase tracking-wider text-slate-500 font-medium px-4 py-3", "Note" }
                                    th { class: "text-right text-[11px] uppercase tracking-wider text-slate-500 font-medium px-4 py-3", "Event ID" }
                                }
                            }
                            tbody {
                                for event in events.iter().rev().take(50) {
                                    {
                                        let origin_short = &event.origin_node_id[..8.min(event.origin_node_id.len())];
                                        let event_id_short = &event.event_id[..8.min(event.event_id.len())];
                                        let note = event.note.as_deref().unwrap_or("-");
                                        let amount_color = if event.amount >= 0 { "text-emerald-400" } else { "text-rose-400" };
                                        let sign = if event.amount >= 0 { "+" } else { "" };
                                        rsx! {
                                            tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                                td { class: "px-4 py-2.5",
                                                    code { class: "text-xs text-slate-400", "{origin_short}..." }
                                                }
                                                td { class: "px-4 py-2.5 text-slate-300", "{event.origin_seq}" }
                                                td { class: "px-4 py-2.5 text-right font-medium {amount_color}", "{sign}{event.amount}" }
                                                td { class: "px-4 py-2.5 text-slate-400", "{note}" }
                                                td { class: "px-4 py-2.5 text-right",
                                                    code { class: "text-xs text-slate-600", "{event_id_short}..." }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
