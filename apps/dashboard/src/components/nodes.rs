use dioxus::prelude::*;
use shardd_types::StateResponse;

use crate::api;

#[component]
pub fn NodeList(states: Vec<(String, StateResponse)>, urls: Vec<String>) -> Element {
    let majority_checksum = {
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (_, s) in &states {
            *counts.entry(s.checksum.as_str()).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by_key(|(_, c)| *c)
            .map(|(cs, _)| cs.to_string())
            .unwrap_or_default()
    };

    rsx! {
        section { class: "mb-8",
            h2 { class: "text-sm font-semibold uppercase tracking-widest text-slate-500 mb-4", "Nodes" }
            div { class: "grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4",
                for (url, state) in &states {
                    NodeCard {
                        key: "{state.node_id}",
                        url: url.clone(),
                        state: state.clone(),
                        majority_checksum: majority_checksum.clone(),
                    }
                }
            }
        }
    }
}

#[component]
fn NodeCard(url: String, state: StateResponse, majority_checksum: String) -> Element {
    let in_sync = state.checksum == majority_checksum;
    let border = if in_sync { "border-l-emerald-500" } else { "border-l-amber-500" };
    let checksum_short = &state.checksum[..16.min(state.checksum.len())];
    let node_id_short = &state.node_id[..8.min(state.node_id.len())];
    let url_for_sync = url.clone();

    rsx! {
        div { class: "bg-slate-900/80 border border-slate-800 {border} border-l-2 rounded-xl p-5 hover:bg-slate-900 transition-colors",
            div { class: "flex items-center justify-between mb-4",
                div {
                    div { class: "text-sm font-semibold text-white", "{state.addr}" }
                    div { class: "text-xs text-slate-500 font-mono", "{node_id_short}..." }
                }
                if in_sync {
                    span { class: "text-[10px] font-medium uppercase tracking-wider bg-emerald-500/10 text-emerald-400 px-2 py-0.5 rounded-full", "synced" }
                } else {
                    span { class: "text-[10px] font-medium uppercase tracking-wider bg-amber-500/10 text-amber-400 px-2 py-0.5 rounded-full", "behind" }
                }
            }
            div { class: "grid grid-cols-2 gap-x-6 gap-y-2 text-sm mb-4",
                div { class: "flex justify-between",
                    span { class: "text-slate-500", "Events" }
                    span { class: "text-white font-medium", "{state.event_count}" }
                }
                div { class: "flex justify-between",
                    span { class: "text-slate-500", "Balance" }
                    span { class: "text-white font-medium", "{state.balance}" }
                }
                div { class: "flex justify-between",
                    span { class: "text-slate-500", "Peers" }
                    span { class: "text-white font-medium", "{state.peers.len()}" }
                }
                div { class: "flex justify-between",
                    span { class: "text-slate-500", "Seq" }
                    span { class: "text-white font-medium", "{state.next_seq}" }
                }
            }
            div { class: "flex items-center justify-between pt-3 border-t border-slate-800",
                code { class: "text-[11px] text-slate-600", "{checksum_short}..." }
                button {
                    class: "text-xs font-medium text-violet-400 hover:text-violet-300 bg-violet-500/10 hover:bg-violet-500/20 px-3 py-1 rounded-md transition-colors",
                    onclick: move |_| {
                        let u = url_for_sync.clone();
                        spawn(async move {
                            let _ = api::trigger_sync(&u).await;
                        });
                    },
                    "Trigger sync"
                }
            }
        }
    }
}
