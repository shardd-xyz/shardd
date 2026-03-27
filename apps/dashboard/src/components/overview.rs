use dioxus::prelude::*;
use shardd_types::StateResponse;

#[component]
pub fn Overview(states: Vec<(String, StateResponse)>) -> Element {
    let node_count = states.len();
    let max_events = states.iter().map(|(_, s)| s.event_count).max().unwrap_or(0);
    let balance = states.first().map(|(_, s)| s.total_balance).unwrap_or(0);

    let checksums: Vec<&str> = states.iter().map(|(_, s)| s.checksum.as_str()).collect();
    let converged = !checksums.is_empty() && checksums.windows(2).all(|w| w[0] == w[1]);

    rsx! {
        section { class: "grid grid-cols-2 sm:grid-cols-4 gap-4 mb-8",
            div { class: "bg-slate-900/80 border border-slate-800 rounded-xl p-5",
                div { class: "text-[11px] uppercase tracking-widest text-slate-500 mb-1", "Nodes" }
                div { class: "text-3xl font-bold text-white", "{node_count}" }
            }
            div { class: "bg-slate-900/80 border border-slate-800 rounded-xl p-5",
                div { class: "text-[11px] uppercase tracking-widest text-slate-500 mb-1", "Events" }
                div { class: "text-3xl font-bold text-white", "{max_events}" }
            }
            div { class: "bg-slate-900/80 border border-slate-800 rounded-xl p-5",
                div { class: "text-[11px] uppercase tracking-widest text-slate-500 mb-1", "Balance" }
                div { class: "text-3xl font-bold",
                    class: if balance >= 0 { "text-emerald-400" } else { "text-rose-400" },
                    "{balance}"
                }
            }
            div {
                class: "bg-slate-900/80 border rounded-xl p-5",
                class: if converged { "border-emerald-500/30" } else { "border-amber-500/30" },
                div { class: "text-[11px] uppercase tracking-widest text-slate-500 mb-1", "Sync" }
                if converged {
                    div { class: "flex items-center gap-2",
                        span { class: "text-3xl font-bold text-emerald-400", "OK" }
                        div { class: "w-3 h-3 rounded-full bg-emerald-400" }
                    }
                } else if states.is_empty() {
                    div { class: "text-3xl font-bold text-slate-600", "--" }
                } else {
                    div { class: "flex items-center gap-2",
                        span { class: "text-3xl font-bold text-amber-400", "Sync" }
                        div { class: "w-3 h-3 rounded-full bg-amber-400 animate-pulse" }
                    }
                }
            }
        }
    }
}
