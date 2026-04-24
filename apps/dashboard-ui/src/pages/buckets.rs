use crate::api;
use crate::components::badge::{Badge, BadgeTone};
use crate::components::button::{Btn, BtnSize, BtnVariant};
use crate::components::pagination::Pagination;
use crate::components::time::*;
use crate::router::Route;
use crate::state::use_notice;
use crate::types::{BucketStatus, BucketSummary, Notice, NoticeTone};
use dioxus::prelude::*;

/// Status filter value submitted to the list endpoint. `""` maps to
/// "all" — the server's default when the param is omitted.
const STATUS_ALL: &str = "";
const STATUS_ACTIVE: &str = "active";
const STATUS_ARCHIVED: &str = "archived";
const STATUS_NUKED: &str = "nuked";

fn status_label(status: BucketStatus) -> (&'static str, BadgeTone) {
    match status {
        BucketStatus::Active => ("Active", BadgeTone::Primary),
        BucketStatus::Archived => ("Archived", BadgeTone::Neutral),
        BucketStatus::Nuked => ("Nuked", BadgeTone::Danger),
    }
}

#[component]
pub fn Buckets() -> Element {
    let mut query = use_signal(String::new);
    let mut status = use_signal(|| STATUS_ALL.to_string());
    let mut page = use_signal(|| 1usize);
    let mut list_loading = use_signal(|| false);
    let mut new_bucket_name = use_signal(String::new);
    let mut creating = use_signal(|| false);
    let mut notice = use_notice();

    let mut data = use_resource(move || {
        let q = query.read().clone();
        let s = status.read().clone();
        let p = *page.read();
        async move {
            let r = api::buckets::list_buckets(&q, p, 15, &s).await.ok();
            list_loading.set(false);
            r
        }
    });

    let on_create = move |evt: FormEvent| {
        evt.prevent_default();
        let name = new_bucket_name.read().trim().to_string();
        if name.is_empty() {
            return;
        }
        creating.set(true);
        spawn(async move {
            match api::buckets::create_bucket(&name).await {
                Ok(_) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Success,
                        "Bucket created",
                        format!(
                            "'{name}' is ready. It will appear below once you write events to it."
                        ),
                    )));
                    new_bucket_name.set(String::new());
                    data.restart();
                }
                Err(e) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Danger,
                        "Bucket creation failed",
                        e.friendly().1,
                    )));
                }
            }
            creating.set(false);
        });
    };

    rsx! {
        div { class: "grid gap-6 w-full",
        section { class: "flex flex-wrap justify-between items-start gap-4",
            div { class: "grid gap-1",
                span { class: "text-accent-100 text-xs uppercase tracking-widest", "Buckets" }
                h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "Bucket explorer" }
            }
            div { class: "flex gap-2.5 items-center",
                if let Some(Some(d)) = &*data.read() {
                    Badge { text: format!("{} buckets", d.total), tone: BadgeTone::Neutral }
                }
                Link { to: Route::Dashboard, class: "px-3.5 py-2 rounded-full border border-base-700 text-base-400 hover:text-fg transition no-underline text-sm", "Back home" }
            }
        }

        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
            h2 { class: "text-[16px] font-mono font-normal text-fg", "Create bucket" }
            form { class: "flex gap-3 items-end",
                onsubmit: on_create,
                div { class: "flex-1 grid gap-1",
                    label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Name" }
                    input {
                        r#type: "text",
                        placeholder: "orders",
                        value: "{new_bucket_name}",
                        oninput: move |e| new_bucket_name.set(e.value()),
                    }
                }
                Btn {
                    r#type: "submit".to_string(),
                    variant: BtnVariant::Primary,
                    size: BtnSize::Default,
                    // Match the adjacent input's height exactly (14px font *
                    // 1.4 line-height + 0.5rem padding × 2 + 1px border × 2).
                    class: "!h-[38px]".to_string(),
                    disabled: *creating.read() || new_bucket_name.read().trim().is_empty(),
                    if *creating.read() { "Creating…" } else { "Create bucket" }
                }
            }
            p { class: "font-mono text-[12px] text-base-500 leading-[140%]",
                "Buckets must be created before your API keys can write events to them. Names are lowercase letters, digits, '-' and '_'."
            }
        }

        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
            div { class: "grid gap-2",
                label { class: "text-base-500 text-xs uppercase tracking-widest font-mono", "Status" }
                div { class: "flex flex-wrap gap-2",
                    {
                        let options: [(&'static str, &'static str); 4] = [
                            (STATUS_ALL, "All"),
                            (STATUS_ACTIVE, "Active"),
                            (STATUS_ARCHIVED, "Archived"),
                            (STATUS_NUKED, "Nuked"),
                        ];
                        let current = status.read().clone();
                        rsx! {
                            for (value, label) in options.iter() {
                                {
                                    let value = value.to_string();
                                    let label = label.to_string();
                                    let active = current == value;
                                    let class = if active {
                                        "px-3 py-1.5 rounded-full border border-accent-100/40 bg-base-1000 text-accent-100 font-mono text-[12px] uppercase tracking-widest transition"
                                    } else {
                                        "px-3 py-1.5 rounded-full border border-base-800 bg-base-900 text-base-500 hover:text-fg font-mono text-[12px] uppercase tracking-widest transition"
                                    };
                                    rsx! {
                                        button {
                                            r#type: "button",
                                            class: class,
                                            onclick: move |_| {
                                                if *status.read() != value {
                                                    status.set(value.clone());
                                                    page.set(1);
                                                }
                                            },
                                            "{label}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            form { class: "flex gap-3",
                onsubmit: move |_| { data.restart(); },
                div { class: "flex-1 grid gap-1",
                    label { class: "text-base-500 text-xs uppercase tracking-widest", "Search buckets" }
                    input {
                        r#type: "search",
                        placeholder: "orders, staging/, droid-smoke…",
                        value: "{query}",
                        oninput: move |e| query.set(e.value()),
                    }
                }
                Btn {
                    r#type: "submit".to_string(),
                    variant: BtnVariant::Primary,
                    size: BtnSize::Default,
                    class: "!h-[38px]".to_string(),
                    "Search"
                }
            }
        }

        section { class: "rounded-lg border border-base-800 bg-base-900 p-6",
            match &*data.read() {
                Some(Some(d)) if d.buckets.is_empty() && query.read().is_empty() && status.read().as_str() == STATUS_ALL => rsx! {
                    crate::components::empty_state::EmptyState {
                        title: "No buckets yet".to_string(),
                        body: "Create your first bucket above, then point an API key at it and write an event.".to_string(),
                        cta: None,
                        external_cta: Some(("Read the quickstart".to_string(), "https://shardd.xyz/guide/quickstart".to_string())),
                    }
                },
                Some(Some(d)) => rsx! {
                    div { class: "overflow-auto rounded-[14px] border border-base-800",
                        table { class: "w-full border-collapse min-w-[680px]",
                            thead { class: "bg-base-1000",
                                tr {
                                    th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Bucket" }
                                    th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Status" }
                                    th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Accounts" }
                                    th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Available" }
                                    th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Last activity" }
                                }
                            }
                            tbody {
                                if d.buckets.is_empty() {
                                    tr { td { colspan: "5", class: "px-4 py-8 text-center text-base-500", "No buckets matched that filter." } }
                                } else {
                                    for bucket in &d.buckets {
                                        BucketRow { bucket: bucket.clone() }
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
                                total: d.total, page: p, total_pages: total_pages, label: "buckets".to_string(),
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

#[component]
fn BucketRow(bucket: BucketSummary) -> Element {
    let (status_text, status_tone) = status_label(bucket.status);
    let row_class = match bucket.status {
        BucketStatus::Active => "border-b border-base-800 hover:bg-base-900",
        BucketStatus::Archived => "border-b border-base-800 text-base-500 hover:bg-base-900",
        BucketStatus::Nuked => "border-b border-base-800 text-base-500",
    };
    let name_class_active = "text-accent-100 hover:text-fg no-underline font-mono text-sm";
    let name_class_archived = "text-base-400 hover:text-fg no-underline font-mono text-sm";
    let name_class_nuked = "text-base-500 font-mono text-sm line-through";

    let accounts_cell: Element = match bucket.account_count {
        Some(n) => rsx! { "{n}" },
        None => rsx! { span { class: "text-base-700", "—" } },
    };
    let balance_cell: Element = match bucket.available_balance {
        Some(v) => {
            rsx! { span { class: "font-mono {amount_class(v)}", "{format_amount(v)}" } }
        }
        None => rsx! { span { class: "text-base-700", "—" } },
    };
    let last_activity_cell: Element = match bucket.status {
        BucketStatus::Nuked => {
            let ts = bucket.deleted_at_unix_ms;
            rsx! {
                span { class: "text-[#f87171]",
                    title: "{format_date(ts)}",
                    "nuked {format_relative_time(ts)}"
                }
            }
        }
        _ => rsx! {
            span {
                title: "{format_date(bucket.last_event_at_unix_ms)}",
                "{format_relative_time(bucket.last_event_at_unix_ms)}"
            }
        },
    };

    rsx! {
        tr { class: row_class,
            td { class: "px-4 py-3",
                match bucket.status {
                    BucketStatus::Nuked => rsx! {
                        span { class: name_class_nuked, "{bucket.bucket}" }
                    },
                    BucketStatus::Archived => rsx! {
                        Link {
                            to: Route::BucketDetail { bucket: bucket.bucket.clone() },
                            class: name_class_archived,
                            "{bucket.bucket}"
                        }
                    },
                    BucketStatus::Active => rsx! {
                        Link {
                            to: Route::BucketDetail { bucket: bucket.bucket.clone() },
                            class: name_class_active,
                            "{bucket.bucket}"
                        }
                    },
                }
            }
            td { class: "px-4 py-3",
                Badge { text: status_text.to_string(), tone: status_tone }
            }
            td { class: "px-4 py-3 text-sm", {accounts_cell} }
            td { class: "px-4 py-3 text-sm", {balance_cell} }
            td { class: "px-4 py-3 text-sm text-base-500", {last_activity_cell} }
        }
    }
}
