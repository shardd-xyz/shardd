use crate::api;
use crate::components::badge::{Badge, BadgeTone};
use crate::components::pagination::Pagination;
use crate::components::time::*;
use dioxus::prelude::*;

#[component]
pub fn AdminAudit() -> Element {
    let mut page = use_signal(|| 1usize);
    let mut list_loading = use_signal(|| false);
    let data = use_resource(move || {
        let p = *page.read();
        async move {
            let r = api::admin::list_audit(p, 15).await.ok();
            list_loading.set(false);
            r
        }
    });

    rsx! {
        div { class: "grid gap-6 w-full",
        section { class: "flex flex-wrap justify-between items-start gap-4",
            div { class: "grid gap-1",
                span { class: "text-accent-100 text-xs uppercase tracking-widest", "Governance" }
                h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "Audit log" }
            }
            if let Some(Some(d)) = &*data.read() {
                Badge { text: format!("{} entries", d.total), tone: BadgeTone::Neutral }
            }
        }

        section { class: "rounded-lg border border-base-800 bg-base-900 p-6",
            match &*data.read() {
                Some(Some(d)) => rsx! {
                    div { class: "overflow-auto rounded-[14px] border border-base-800",
                        table { class: "w-full border-collapse min-w-[700px]",
                            thead { class: "bg-base-1000",
                                tr {
                                    th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "When" }
                                    th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Admin" }
                                    th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Action" }
                                    th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Target" }
                                    th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Metadata" }
                                }
                            }
                            tbody {
                                for entry in &d.entries {
                                    tr { class: "border-b border-base-800 hover:bg-base-900",
                                        td { class: "px-4 py-3 text-sm text-base-500",
                                            title: "{format_date_str(Some(&entry.created_at))}",
                                            "{format_relative_time_str(Some(&entry.created_at))}"
                                        }
                                        td { class: "px-4 py-3 text-sm", "{entry.admin_email}" }
                                        td { class: "px-4 py-3",
                                            span { class: "inline-block px-2 py-0.5 rounded-full bg-base-1000 border border-base-800 text-base-300 text-xs font-mono",
                                                "{entry.action}"
                                            }
                                        }
                                        td { class: "px-4 py-3",
                                            span { class: "inline-block max-w-[26ch] overflow-hidden text-ellipsis whitespace-nowrap align-middle text-sm",
                                                title: "{entry.target_email.as_deref().or(entry.target_user_id.as_deref()).unwrap_or(\"—\")}",
                                                if let Some(email) = &entry.target_email {
                                                    span { class: "text-accent-100", "{email}" }
                                                } else if let Some(uid) = &entry.target_user_id {
                                                    span { class: "text-accent-100", "{uid}" }
                                                } else {
                                                    "—"
                                                }
                                            }
                                        }
                                        td { class: "px-4 py-3 text-sm",
                                            if entry.metadata.as_ref().map(|m| m.as_object().map(|o| !o.is_empty()).unwrap_or(false)).unwrap_or(false) {
                                                details { class: "cursor-pointer",
                                                    summary { class: "text-base-500 underline decoration-slate-700 underline-offset-2 hover:text-base-300 text-sm",
                                                        "View metadata"
                                                    }
                                                    div { class: "mt-2 p-3 rounded-lg bg-base-1000 border border-base-800 text-xs font-mono text-base-400 whitespace-pre-wrap",
                                                        "{serde_json::to_string_pretty(entry.metadata.as_ref().unwrap()).unwrap_or_default()}"
                                                    }
                                                }
                                            } else {
                                                "—"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    {
                        let total_pages = d.total.div_ceil(15);
                        let p = *page.read();
                        rsx! {
                            Pagination {
                                total: d.total, page: p, total_pages: total_pages, label: "entries".to_string(),
                                loading: *list_loading.read(),
                                on_prev: move |_| { list_loading.set(true); page.set(p - 1); },
                                on_next: move |_| { list_loading.set(true); page.set(p + 1); },
                            }
                        }
                    }
                },
                _ => rsx! { div { class: "text-base-500 text-center py-8", "Loading…" } },
            }
        }
        }
    }
}
