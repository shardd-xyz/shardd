use crate::api;
use crate::components::badge::{Badge, BadgeTone};
use crate::components::pagination::Pagination;
use crate::components::time::*;
use crate::router::Route;
use dioxus::prelude::*;

#[component]
pub fn AdminUsers() -> Element {
    let mut query = use_signal(String::new);
    let mut status = use_signal(|| "active".to_string());
    let mut page = use_signal(|| 1usize);
    let mut list_loading = use_signal(|| false);
    let mut data = use_resource(move || {
        let q = query.read().clone();
        let s = status.read().clone();
        let p = *page.read();
        async move {
            let r = api::admin::list_users(&q, &s, p, 15).await.ok();
            list_loading.set(false);
            r
        }
    });

    rsx! {
        div { class: "grid gap-6 w-full",
        section { class: "flex flex-wrap justify-between items-start gap-4",
            div { class: "grid gap-1",
                span { class: "text-accent-100 font-mono text-[12px] uppercase tracking-[-0.015rem]", "Admin" }
                h1 { class: "text-[32px] font-mono font-normal leading-[100%] tracking-[-0.04em] text-fg", "Users" }
            }
            if let Some(Some(d)) = &*data.read() {
                Badge { text: format!("{} total", d.total), tone: BadgeTone::Neutral }
            }
        }

        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-3",
            form { class: "flex gap-3",
                onsubmit: move |_| { page.set(1); data.restart(); },
                div { class: "flex-1 grid gap-1",
                    label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Search users" }
                    input { r#type: "search", placeholder: "Search by email", value: "{query}", oninput: move |e| query.set(e.value()) }
                }
                button { r#type: "submit", class: "group relative h-[31px] px-[14px] font-mono text-[12px] uppercase tracking-[-0.015rem] bg-[var(--btn-primary-bg)] text-[var(--btn-primary-text)] border border-base-700 rounded-sm overflow-hidden transition-colors duration-150 hover:opacity-80 self-end",
                    "Search"
                }
            }
            nav { class: "flex gap-2",
                for (value, label) in [("active", "Active"), ("deleted", "Deleted"), ("all", "All")] {
                    button {
                        class: if *status.read() == value {
                            "px-2 py-0.5 rounded font-mono text-[11px] uppercase tracking-[-0.01rem] border border-base-800 bg-base-1000 text-fg transition-colors"
                        } else {
                            "px-2 py-0.5 rounded font-mono text-[11px] uppercase tracking-[-0.01rem] border border-transparent text-base-500 hover:text-fg transition-colors"
                        },
                        onclick: move |_| {
                            status.set(value.to_string());
                            page.set(1);
                        },
                        "{label}"
                    }
                }
            }
        }

        section { class: "rounded-lg border border-base-800 bg-base-900 p-6",
            match &*data.read() {
                Some(Some(d)) => rsx! {
                    div { class: "overflow-auto rounded-lg border border-base-800",
                        table { class: "w-full border-collapse min-w-[600px]",
                            thead { class: "bg-base-1000",
                                tr {
                                    th { class: "px-4 py-3 text-left font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Email" }
                                    th { class: "px-4 py-3 text-left font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Status" }
                                    th { class: "px-4 py-3 text-left font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Last login" }
                                }
                            }
                            tbody {
                                for user in &d.users {
                                    tr { class: "border-b border-base-800 hover:bg-base-900",
                                        td { class: "px-4 py-3",
                                            Link {
                                                to: Route::AdminUser { user_id: user.id.clone() },
                                                class: "text-accent-100 hover:text-fg no-underline font-mono text-[14px] max-w-[26ch] inline-block overflow-hidden text-ellipsis whitespace-nowrap align-middle",
                                                title: "{user.email}",
                                                "{user.email}"
                                            }
                                        }
                                        td { class: "px-4 py-3",
                                            div { class: "flex gap-2 flex-wrap",
                                                if user.deleted_at.is_some() {
                                                    Badge { text: "deleted".to_string(), tone: BadgeTone::Danger }
                                                } else if user.is_admin {
                                                    Badge { text: "admin".to_string(), tone: BadgeTone::Primary }
                                                } else {
                                                    Badge { text: "developer".to_string(), tone: BadgeTone::Neutral }
                                                }
                                                if user.deleted_at.is_none() {
                                                    if user.is_frozen {
                                                        Badge { text: "frozen".to_string(), tone: BadgeTone::Warning }
                                                    } else {
                                                        Badge { text: "active".to_string(), tone: BadgeTone::Success }
                                                    }
                                                }
                                            }
                                        }
                                        {
                                            let (title, body) = if let Some(ts) = user.deleted_at.as_deref() {
                                                (
                                                    format_date_str(Some(ts)),
                                                    format!("deleted {}", format_relative_time_str(Some(ts))),
                                                )
                                            } else {
                                                (
                                                    format_date_str(user.last_login_at.as_deref()),
                                                    format_relative_time_str(user.last_login_at.as_deref()),
                                                )
                                            };
                                            rsx! {
                                                td {
                                                    class: "px-4 py-3 font-mono text-[14px] text-base-500",
                                                    title: "{title}",
                                                    "{body}"
                                                }
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
                                total: d.total, page: p, total_pages: total_pages, label: "users".to_string(),
                                loading: *list_loading.read(),
                                on_prev: move |_| { list_loading.set(true); page.set(p - 1); },
                                on_next: move |_| { list_loading.set(true); page.set(p + 1); },
                            }
                        }
                    }
                },
                _ => rsx! { div { class: "text-base-500 text-center py-8 font-mono text-[14px]", "Loading…" } },
            }
        }
        }
    }
}
