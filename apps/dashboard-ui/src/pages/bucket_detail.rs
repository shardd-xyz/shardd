use crate::api;
use crate::components::badge::{Badge, BadgeTone};
use crate::components::button::{Btn, BtnSize, BtnVariant};
use crate::components::copy_button::CopyButton;
use crate::components::event_card::EventCard;
use crate::components::meta_row::{MetaRow, MetaRowCode};
use crate::components::pagination::Pagination;
use crate::components::time::*;
use crate::router::Route;
use crate::state::use_notice;
use crate::types::{CreateEventRequest, Notice, NoticeTone};
use dioxus::prelude::*;

#[component]
pub fn BucketDetail(bucket: String) -> Element {
    let mut tab = use_signal(|| "events".to_string());
    let mut search_q = use_signal(String::new);
    let mut search_account = use_signal(String::new);
    let mut page = use_signal(|| 1usize);
    let mut events_loading = use_signal(|| false);
    let bucket_clone = bucket.clone();

    let mut detail = use_resource({
        let bucket = bucket.clone();
        move || {
            let bucket = bucket.clone();
            async move { api::buckets::get_bucket_detail(&bucket).await.ok() }
        }
    });

    let mut events = use_resource(move || {
        let bucket = bucket_clone.clone();
        let q = search_q.read().clone();
        let acc = search_account.read().clone();
        let p = *page.read();
        let t = tab.read().clone();
        async move {
            let result = if t == "events" {
                api::buckets::list_bucket_events(&bucket, &q, &acc, p, 10)
                    .await
                    .ok()
            } else {
                None
            };
            events_loading.set(false);
            result
        }
    });

    rsx! {
        section { class: "flex flex-wrap justify-between items-start gap-4",
            div { class: "grid gap-1",
                span { class: "text-accent-100 text-xs uppercase tracking-widest", "Bucket" }
                h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "{bucket}" }
            }
            div { class: "flex gap-2.5",
                Link { to: Route::Buckets, class: "px-3.5 py-2 rounded-full border border-base-700 text-base-400 hover:text-fg transition no-underline text-sm", "All buckets" }
            }
        }

        QuickstartSnippet { bucket: bucket.clone() }

        section { class: "grid grid-cols-[minmax(0,1fr)_minmax(280px,320px)] gap-5 items-start max-[960px]:grid-cols-1",
            // Main content
            div { class: "grid gap-5",
                // Tabs
                nav { class: "flex gap-2",
                    button {
                        class: if *tab.read() == "events" { "px-3.5 py-2 rounded-full text-sm text-fg bg-base-900 border border-base-700" } else { "px-3.5 py-2 rounded-full text-sm text-base-500 border border-transparent hover:text-fg" },
                        onclick: move |_| tab.set("events".to_string()),
                        "Events"
                    }
                    button {
                        class: if *tab.read() == "accounts" { "px-3.5 py-2 rounded-full text-sm text-fg bg-base-900 border border-base-700" } else { "px-3.5 py-2 rounded-full text-sm text-base-500 border border-transparent hover:text-fg" },
                        onclick: move |_| tab.set("accounts".to_string()),
                        "Accounts"
                    }
                }

                if *tab.read() == "events" {
                    // Events panel
                    section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-5",
                        div { class: "flex justify-between items-start",
                            h2 { class: "text-[16px] font-normal", "Events" }
                            if let Some(Some(ev)) = &*events.read() {
                                Badge { text: format!("{} matches", ev.total), tone: BadgeTone::Neutral }
                            }
                        }
                        form { class: "flex gap-3 flex-wrap",
                            onsubmit: move |_| { events.restart(); },
                            div { class: "flex-1 min-w-[200px] grid gap-1",
                                label { class: "text-base-500 text-xs uppercase tracking-widest", "Search events" }
                                input { r#type: "search", placeholder: "event id, note, nonce, amount…", value: "{search_q}", oninput: move |e| search_q.set(e.value()) }
                            }
                            div { class: "flex-1 min-w-[120px] grid gap-1",
                                label { class: "text-base-500 text-xs uppercase tracking-widest", "Account filter" }
                                input { r#type: "search", placeholder: "main", value: "{search_account}", oninput: move |e| search_account.set(e.value()) }
                            }
                            button { r#type: "submit", class: "bg-[var(--btn-primary-bg)] hover:bg-base-800 text-[var(--btn-primary-text)] font-normal px-5 py-2 rounded-lg transition self-end", "Search" }
                        }
                        match &*events.read() {
                            Some(Some(ev)) => rsx! {
                                if ev.events.is_empty() {
                                    div { class: "py-7 text-center text-base-500 border border-dashed border-base-800 rounded-[14px] font-mono text-[14px]", "No bucket events matched that search." }
                                } else {
                                    div { class: "grid gap-2.5",
                                        for event in &ev.events {
                                            EventCard { event: event.clone() }
                                        }
                                    }
                                    {
                                        let total_pages = ev.total.div_ceil(10);
                                        let p = *page.read();
                                        rsx! {
                                            Pagination {
                                                total: ev.total, page: p, total_pages: total_pages, label: "events".to_string(),
                                                loading: *events_loading.read(),
                                                on_prev: move |_| { events_loading.set(true); page.set(p - 1); },
                                                on_next: move |_| { events_loading.set(true); page.set(p + 1); },
                                            }
                                        }
                                    }
                                }
                            },
                            _ => rsx! { div { class: "text-base-500 text-center py-8 font-mono text-[14px]", "Loading…" } },
                        }
                    }
                } else {
                    // Accounts panel
                    section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-5",
                        div { class: "flex justify-between items-start",
                            h2 { class: "text-[16px] font-normal", "Accounts" }
                            if let Some(Some(d)) = &*detail.read() {
                                Badge { text: format!("{} accounts", d.accounts.len()), tone: BadgeTone::Neutral }
                            }
                        }
                        match &*detail.read() {
                            Some(Some(d)) => rsx! {
                                div { class: "overflow-auto rounded-[14px] border border-base-800",
                                    table { class: "w-full border-collapse min-w-[600px]",
                                        thead { class: "bg-base-1000",
                                            tr {
                                                th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Account" }
                                                th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Balance" }
                                                th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Available" }
                                                th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Active holds" }
                                                th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Events" }
                                                th { class: "px-4 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Last activity" }
                                            }
                                        }
                                        tbody {
                                            for acc in &d.accounts {
                                                {
                                                    let bucket_for_link = bucket.clone();
                                                    rsx! {
                                                        tr { class: "border-b border-base-800 hover:bg-base-900",
                                                            td { class: "px-4 py-3",
                                                                Link { to: Route::AccountDetail { bucket: bucket_for_link, account: acc.account.clone() }, class: "text-accent-100 hover:text-fg no-underline font-mono text-sm", "{acc.account}" }
                                                            }
                                                            td { class: "px-4 py-3 text-sm font-mono {amount_class(acc.balance)}", "{format_amount(acc.balance)}" }
                                                            td { class: "px-4 py-3 text-sm font-mono {amount_class(acc.available_balance)}", "{format_amount(acc.available_balance)}" }
                                                            td { class: "px-4 py-3 text-sm font-mono", "{format_amount(acc.active_hold_total)}" }
                                                            td { class: "px-4 py-3 text-sm", "{acc.event_count}" }
                                                            td { class: "px-4 py-3 text-sm text-base-500",
                                                                title: "{format_date(acc.last_event_at_unix_ms)}",
                                                                "{format_relative_time(acc.last_event_at_unix_ms)}"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            _ => rsx! { div { class: "text-base-500 text-center py-8", "Loading…" } },
                        }
                    }
                }
            }

            // Sidebar
            aside { class: "grid gap-5",
                WriteEventForm { bucket: bucket.clone(), on_written: move |_| { detail.restart(); events.restart(); } }

                // Bucket summary
                if let Some(Some(d)) = &*detail.read() {
                    section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
                        h2 { class: "text-[16px] font-normal", "Bucket summary" }
                        div {
                            MetaRowCode { label: "Bucket".to_string(), value: d.summary.bucket.clone() }
                            MetaRow { label: "Accounts".to_string(), value: d.summary.account_count.to_string() }
                            MetaRow { label: "Events".to_string(), value: d.summary.event_count.to_string() }
                            MetaRow { label: "Available".to_string(), value: format_amount(d.summary.available_balance) }
                            MetaRow { label: "Active holds".to_string(), value: format_amount(d.summary.active_hold_total) }
                            MetaRow { label: "Last activity".to_string(), value: format_relative_time(d.summary.last_event_at_unix_ms) }
                        }
                    }
                }

                DangerZone { bucket: bucket.clone() }
            }
        }
    }
}

#[component]
fn DangerZone(bucket: String) -> Element {
    // Archive (soft-hide) and Permanuke (cluster-wide hard-delete) live
    // in the same zone. Each has its own expanded/typed-confirm state.
    let mut archive_confirm = use_signal(String::new);
    let mut archive_expanded = use_signal(|| false);
    let mut archive_submitting = use_signal(|| false);
    let mut purge_confirm = use_signal(String::new);
    let mut purge_expanded = use_signal(|| false);
    let mut purge_submitting = use_signal(|| false);
    let mut notice = use_notice();
    let nav = use_navigator();

    let archive_bucket_for_check = bucket.clone();
    let archive_matches = *archive_confirm.read() == archive_bucket_for_check;
    let purge_bucket_for_check = bucket.clone();
    let purge_matches = *purge_confirm.read() == purge_bucket_for_check;

    let on_archive = {
        let bucket = bucket.clone();
        move |_| {
            let bucket = bucket.clone();
            archive_submitting.set(true);
            spawn(async move {
                match api::buckets::archive_bucket(&bucket).await {
                    Ok(_) => {
                        notice.set(Some(Notice::new(
                            NoticeTone::Success,
                            "Bucket archived",
                            format!("'{bucket}' is hidden and no longer accepts writes. Event history stays on the mesh."),
                        )));
                        nav.push(Route::Buckets);
                    }
                    Err(e) => {
                        notice.set(Some(Notice::new(
                            NoticeTone::Danger,
                            "Bucket archive failed",
                            e.friendly().1,
                        )));
                    }
                }
                archive_submitting.set(false);
            });
        }
    };

    let on_purge = {
        let bucket = bucket.clone();
        move |_| {
            let bucket = bucket.clone();
            purge_submitting.set(true);
            spawn(async move {
                match api::buckets::purge_bucket(&bucket).await {
                    Ok(_) => {
                        notice.set(Some(Notice::new(
                            NoticeTone::Success,
                            "Bucket deleted permanently",
                            format!(
                                "'{bucket}' was wiped cluster-wide. The name is reserved and can never be reused."
                            ),
                        )));
                        nav.push(Route::Buckets);
                    }
                    Err(e) => {
                        notice.set(Some(Notice::new(
                            NoticeTone::Danger,
                            "Permanent delete failed",
                            e.friendly().1,
                        )));
                    }
                }
                purge_submitting.set(false);
            });
        }
    };

    rsx! {
        section { class: "rounded-lg border border-accent-200/30 bg-base-900 p-6 grid gap-5",
            h2 { class: "text-[16px] font-normal text-accent-200", "Danger zone" }

            // ── Archive (soft-hide) ──────────────────────────────
            div { class: "grid gap-3",
                if !*archive_expanded.read() {
                    p { class: "font-mono text-[12px] text-base-500 leading-[140%]",
                        "Archive this bucket. It disappears from your list and rejects new events. "
                        "Event history stays on the mesh \u{2014} this is the reversible option."
                    }
                    div {
                        Btn {
                            variant: BtnVariant::Danger,
                            size: BtnSize::Sm,
                            onclick: move |_| archive_expanded.set(true),
                            "Archive bucket"
                        }
                    }
                } else {
                    p { class: "font-mono text-[12px] text-base-400 leading-[140%]",
                        "Type the bucket name to confirm archive:"
                    }
                    input {
                        r#type: "text",
                        placeholder: "{bucket}",
                        value: "{archive_confirm}",
                        oninput: move |e| archive_confirm.set(e.value()),
                    }
                    div { class: "flex items-center gap-2",
                        Btn {
                            variant: BtnVariant::Ghost,
                            size: BtnSize::Sm,
                            disabled: *archive_submitting.read(),
                            onclick: move |_| {
                                archive_expanded.set(false);
                                archive_confirm.set(String::new());
                            },
                            "Cancel"
                        }
                        Btn {
                            variant: BtnVariant::Danger,
                            size: BtnSize::Sm,
                            disabled: !archive_matches || *archive_submitting.read(),
                            onclick: on_archive,
                            if *archive_submitting.read() { "Archiving\u{2026}" } else { "Archive bucket" }
                        }
                    }
                }
            }

            // ── Permanuke (cluster-wide hard-delete) ────────────
            div { class: "grid gap-3 rounded border border-accent-200/50 p-4",
                h3 { class: "text-[14px] font-normal text-accent-200", "Permanently delete" }
                if !*purge_expanded.read() {
                    p { class: "font-mono text-[12px] text-base-500 leading-[140%]",
                        "Hard-delete this bucket across every region. Every event is wiped from "
                        "all nodes. The name is reserved forever \u{2014} you cannot re-create a "
                        "bucket with this name. "
                        strong { class: "text-accent-200", "This cannot be undone." }
                    }
                    div {
                        Btn {
                            variant: BtnVariant::Danger,
                            size: BtnSize::Sm,
                            onclick: move |_| purge_expanded.set(true),
                            "Permanently delete\u{2026}"
                        }
                    }
                } else {
                    p { class: "font-mono text-[12px] text-base-400 leading-[140%]",
                        "Type the bucket name to confirm permanent deletion. After this:"
                    }
                    ul { class: "font-mono text-[12px] text-base-500 leading-[150%] ml-4 list-disc",
                        li { "Every event in '{bucket}' is wiped cluster-wide (eventually consistent via gossip)." }
                        li { "The name '{bucket}' is reserved \u{2014} no new bucket can ever use it." }
                        li { "There is no undelete, no grace period, no restore." }
                    }
                    input {
                        r#type: "text",
                        placeholder: "{bucket}",
                        value: "{purge_confirm}",
                        oninput: move |e| purge_confirm.set(e.value()),
                    }
                    div { class: "flex items-center gap-2",
                        Btn {
                            variant: BtnVariant::Ghost,
                            size: BtnSize::Sm,
                            disabled: *purge_submitting.read(),
                            onclick: move |_| {
                                purge_expanded.set(false);
                                purge_confirm.set(String::new());
                            },
                            "Cancel"
                        }
                        Btn {
                            variant: BtnVariant::Danger,
                            size: BtnSize::Sm,
                            disabled: !purge_matches || *purge_submitting.read(),
                            onclick: on_purge,
                            if *purge_submitting.read() { "Deleting\u{2026}" } else { "Permanuke this bucket" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn WriteEventForm(bucket: String, on_written: EventHandler<()>) -> Element {
    let mut account = use_signal(String::new);
    let mut amount = use_signal(String::new);
    let mut note = use_signal(String::new);
    let mut idempotency = use_signal(String::new);
    let mut notice = use_notice();
    let bucket_dep = bucket.clone();
    let bucket_chg = bucket.clone();

    let mut do_submit = move |bucket: String, direction: &'static str| {
        let acc = account.read().clone();
        let amt_str = amount.read().clone();
        let n = note.read().clone();
        let idem = idempotency.read().clone();

        let amt: i64 = match amt_str.parse::<i64>() {
            Ok(v) if v > 0 => {
                if direction == "charge" {
                    -v
                } else {
                    v
                }
            }
            _ => {
                notice.set(Some(Notice::new(
                    NoticeTone::Danger,
                    "Invalid amount",
                    "Amount must be a positive whole number.",
                )));
                return;
            }
        };
        if acc.trim().is_empty() {
            notice.set(Some(Notice::new(
                NoticeTone::Danger,
                "Missing account",
                "Enter an account name.",
            )));
            return;
        }

        spawn(async move {
            let req = CreateEventRequest {
                account: acc,
                amount: amt,
                note: if n.is_empty() { None } else { Some(n) },
                // Every event now carries a nonce. If the operator typed
                // one, honor it; otherwise generate a UUID per submit so
                // the write still satisfies the "always deduped" invariant.
                idempotency_nonce: if idem.is_empty() {
                    Some(uuid::Uuid::new_v4().to_string())
                } else {
                    Some(idem)
                },
                max_overdraft: None,
                min_acks: None,
                ack_timeout_ms: None,
            };
            let label = if direction == "charge" {
                "Charge"
            } else {
                "Deposit"
            };
            match api::buckets::create_event(&bucket, &req).await {
                Ok(_) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Success,
                        format!("{label} created"),
                        format!("Posted to {bucket}."),
                    )));
                    account.set(String::new());
                    amount.set(String::new());
                    note.set(String::new());
                    idempotency.set(String::new());
                    on_written.call(());
                }
                Err(e) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Danger,
                        format!("{label} failed"),
                        e.friendly().1,
                    )));
                }
            }
        });
    };

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
            h2 { class: "text-[16px] font-mono font-normal text-fg", "Deposit or charge" }
            div { class: "grid gap-3",
                div { class: "grid gap-1",
                    label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Account" }
                    input { r#type: "text", placeholder: "main", value: "{account}", oninput: move |e| account.set(e.value()) }
                }
                div { class: "grid gap-1",
                    label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Amount" }
                    input { r#type: "number", min: "1", step: "1", placeholder: "1000", value: "{amount}", oninput: move |e| amount.set(e.value()) }
                }
                div { class: "grid gap-1",
                    label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Note" }
                    input { r#type: "text", placeholder: "Optional operator note", value: "{note}", oninput: move |e| note.set(e.value()) }
                }
                div { class: "grid gap-1",
                    label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Idempotency key" }
                    input { r#type: "text", placeholder: "Optional", value: "{idempotency}", oninput: move |e| idempotency.set(e.value()) }
                }
                div { class: "flex gap-2",
                    Btn { variant: BtnVariant::Primary, size: BtnSize::Default, onclick: move |_| do_submit(bucket_dep.clone(), "deposit"), "Deposit" }
                    Btn { variant: BtnVariant::Primary, size: BtnSize::Default, onclick: move |_| do_submit(bucket_chg.clone(), "charge"), "Charge" }
                }
            }
        }
    }
}

/// "Write your first event" snippet. Pre-fills the user's nearest public edge
/// (from /api/developer/edges) and the current bucket name; API key is a
/// `$SHARDD_API_KEY` placeholder the dev fills in. Text is a close mirror of
/// shardd.xyz/guide/quickstart so docs + dashboard stay in sync.
#[component]
fn QuickstartSnippet(bucket: String) -> Element {
    let edges = use_resource(|| async { api::buckets::list_edges().await.unwrap_or_default() });
    let edges_read = edges.read();
    let edge_url = edges_read
        .as_ref()
        .and_then(|list| list.first())
        .map(|e| e.base_url.trim_end_matches('/').to_string())
        .unwrap_or_else(|| "https://use1.api.shardd.xyz".to_string());

    let curl = format!(
        "curl -sS {edge}/events \\\n  -H \"Authorization: Bearer $SHARDD_API_KEY\" \\\n  -H \"Content-Type: application/json\" \\\n  -d '{{\"bucket\":\"{bucket}\",\"account\":\"alice\",\"amount\":100,\"note\":\"top-up\"}}'",
        edge = edge_url
    );

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-3",
            div { class: "flex justify-between items-start flex-wrap gap-2",
                div { class: "grid gap-1",
                    span { class: "text-accent-100 text-xs uppercase tracking-widest", "Get started" }
                    h2 { class: "text-[16px] font-normal", "Write your first event" }
                }
                CopyButton { value: curl.clone(), label: Some("Copy curl".to_string()) }
            }
            pre { class: "p-4 rounded-lg bg-base-1000 border border-base-800 font-mono text-[13px] text-base-300 overflow-x-auto leading-[160%] m-0",
                "{curl}"
            }
            div { class: "flex flex-wrap gap-4 items-center text-base-500 font-mono text-[12px]",
                span { "Set " code { class: "text-accent-100", "SHARDD_API_KEY" } " to a key from " Link { to: Route::Keys, class: "text-accent-100 hover:text-fg no-underline", "/keys" } "." }
                a {
                    href: "https://shardd.xyz/guide/quickstart",
                    target: "_blank",
                    rel: "noopener",
                    class: "text-base-500 hover:text-fg transition-colors duration-150 no-underline ml-auto",
                    "Full quickstart \u{2197}"
                }
            }
        }
    }
}
