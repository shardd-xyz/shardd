use crate::api;
use crate::api::ApiError;
use crate::components::badge::{Badge, BadgeTone, event_type_badge};
use crate::components::pagination::Pagination;
use crate::components::time::*;
use crate::types::*;
use dioxus::prelude::*;

const PAGE_SIZE: usize = 25;

/// Which backend API the events view should talk to. The admin scope hits
/// `/api/admin/events` (cluster-wide, internal bucket names visible);
/// the developer scope hits `/api/developer/events` (filtered to the
/// caller's buckets, with user-facing bucket names).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventsScope {
    Admin,
    Developer,
}

impl EventsScope {
    async fn fetch(
        self,
        filter: &AdminEventsFilter,
        page: usize,
        limit: usize,
        replication: bool,
    ) -> Result<AdminEventListResponse, ApiError> {
        match self {
            EventsScope::Admin => api::admin::list_events(filter, page, limit, replication).await,
            EventsScope::Developer => {
                api::events::list_events(filter, page, limit, replication).await
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct FilterDraft {
    bucket: String,
    account: String,
    origin: String,
    event_type: String,
    since: String,
    until: String,
    search: String,
}

impl FilterDraft {
    fn to_filter(&self) -> AdminEventsFilter {
        AdminEventsFilter {
            bucket: self.bucket.trim().to_string(),
            account: self.account.trim().to_string(),
            origin: self.origin.trim().to_string(),
            event_type: self.event_type.trim().to_string(),
            since_ms: parse_datetime_local_to_ms(self.since.trim()),
            until_ms: parse_datetime_local_to_ms(self.until.trim()),
            search: self.search.trim().to_string(),
        }
    }
}

#[component]
pub fn AdminEvents() -> Element {
    rsx! { EventsView { scope: EventsScope::Admin } }
}

#[component]
pub fn EventsView(scope: EventsScope) -> Element {
    let mut draft = use_signal(FilterDraft::default);
    let mut applied = use_signal(AdminEventsFilter::default);
    let mut page = use_signal(|| 1usize);
    let mut list_loading = use_signal(|| false);
    let mut expanded = use_signal(|| Option::<String>::None);
    let mut replication_snapshot = use_signal(|| Option::<ReplicationSnapshot>::None);
    let mut replication_loading = use_signal(|| false);

    let data = use_resource(move || {
        let f = applied.read().clone();
        let p = *page.read();
        async move {
            let r = scope.fetch(&f, p, PAGE_SIZE, false).await.ok();
            list_loading.set(false);
            r
        }
    });

    rsx! {
        div { class: "grid gap-6 w-full",
            section { class: "flex flex-wrap justify-between items-start gap-4",
                div { class: "grid gap-1",
                    span { class: "text-accent-100 text-xs uppercase tracking-widest", "Observability" }
                    h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "Events" }
                }
                if let Some(Some(d)) = &*data.read() {
                    Badge { text: format!("{} matching", d.total), tone: BadgeTone::Neutral }
                }
            }

            // ── Filter bar ─────────────────────────────────────
            section { class: "rounded-lg border border-base-800 bg-base-900 p-4",
                form {
                    class: "grid gap-3",
                    onsubmit: move |evt| {
                        evt.prevent_default();
                        list_loading.set(true);
                        applied.set(draft.read().to_filter());
                        page.set(1);
                    },
                    div { class: "grid grid-cols-2 md:grid-cols-3 lg:grid-cols-4 gap-2",
                        FilterInput { label: "Bucket", value: draft.read().bucket.clone(), placeholder: "any".to_string(),
                            on_input: move |v: String| draft.write().bucket = v }
                        FilterInput { label: "Account", value: draft.read().account.clone(), placeholder: "any".to_string(),
                            on_input: move |v: String| draft.write().account = v }
                        FilterInput { label: "Origin node", value: draft.read().origin.clone(), placeholder: "any".to_string(),
                            on_input: move |v: String| draft.write().origin = v }
                        div { class: "grid gap-1",
                            label { class: "text-[11px] uppercase tracking-widest text-base-500 font-mono", "Type" }
                            select {
                                class: "w-full px-2 py-1 rounded border border-base-800 bg-base-1000 text-sm font-mono text-fg",
                                value: "{draft.read().event_type}",
                                oninput: move |evt| draft.write().event_type = evt.value(),
                                option { value: "", "any" }
                                option { value: "standard", "standard" }
                                option { value: "reservation_create", "reservation_create" }
                                option { value: "void", "void" }
                                option { value: "hold_release", "hold_release" }
                                option { value: "bucket_delete", "bucket_delete" }
                            }
                        }
                        FilterInputDateTime { label: "Since", value: draft.read().since.clone(),
                            on_input: move |v: String| draft.write().since = v }
                        FilterInputDateTime { label: "Until", value: draft.read().until.clone(),
                            on_input: move |v: String| draft.write().until = v }
                        div { class: "grid gap-1 col-span-2",
                            label { class: "text-[11px] uppercase tracking-widest text-base-500 font-mono", "Search (note or event_id substring)" }
                            input {
                                class: "w-full px-2 py-1 rounded border border-base-800 bg-base-1000 text-sm font-mono text-fg",
                                value: "{draft.read().search}",
                                oninput: move |evt| draft.write().search = evt.value(),
                            }
                        }
                    }
                    div { class: "flex gap-2",
                        button {
                            r#type: "submit",
                            class: "px-3 py-1 rounded border border-accent-100/40 bg-base-1000 text-accent-100 text-xs font-mono uppercase tracking-widest hover:bg-accent-100/10 transition-colors",
                            "Apply"
                        }
                        button {
                            r#type: "button",
                            class: "px-3 py-1 rounded border border-base-800 bg-base-1000 text-base-400 text-xs font-mono uppercase tracking-widest hover:text-fg transition-colors",
                            onclick: move |_| {
                                draft.set(FilterDraft::default());
                                list_loading.set(true);
                                applied.set(AdminEventsFilter::default());
                                page.set(1);
                            },
                            "Clear"
                        }
                    }
                }
            }

            // ── Table ──────────────────────────────────────────
            section { class: "rounded-lg border border-base-800 bg-base-900 p-6",
                match &*data.read() {
                    Some(Some(d)) => rsx! {
                        div { class: "overflow-auto rounded-[14px] border border-base-800",
                            table { class: "w-full border-collapse min-w-[900px]",
                                thead { class: "bg-base-1000",
                                    tr {
                                        th { class: "px-3 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "" }
                                        th { class: "px-3 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "When" }
                                        th { class: "px-3 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Bucket" }
                                        th { class: "px-3 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Origin:Epoch" }
                                        th { class: "px-3 py-3 text-right text-base-500 uppercase tracking-widest text-xs font-mono", "Seq" }
                                        th { class: "px-3 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Type" }
                                        th { class: "px-3 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Account" }
                                        th { class: "px-3 py-3 text-right text-base-500 uppercase tracking-widest text-xs font-mono", "Amount" }
                                        th { class: "px-3 py-3 text-left text-base-500 uppercase tracking-widest text-xs font-mono", "Note" }
                                        th { class: "px-3 py-3 text-center text-base-500 uppercase tracking-widest text-xs font-mono", "Repl" }
                                    }
                                }
                                tbody {
                                    if d.events.is_empty() {
                                        tr {
                                            td { colspan: 10, class: "px-3 py-6 text-center text-base-500 text-sm", "No events match the filter." }
                                        }
                                    }
                                    for event in d.events.clone().into_iter() {
                                        {
                                            let ev = event;
                                            let event_id = ev.event_id.clone();
                                            let event_id_for_toggle = event_id.clone();
                                            let is_open = expanded.read().as_deref() == Some(event_id.as_str());
                                            let origin_short = short_origin(&ev.origin_node_id);
                                            let note_txt = ev.note.clone().unwrap_or_default();
                                            let key = format!("{}\t{}:{}", ev.bucket, ev.origin_node_id, ev.origin_epoch);
                                            let head = d.heads.get(&key).copied().unwrap_or(0);
                                            let max_known = d.max_known_seqs.get(&key).copied().unwrap_or(head);
                                            let (dot_color, dot_title) = replication_dot(ev.origin_seq, head, max_known);
                                            rsx! {
                                                tr {
                                                    key: "{event_id}",
                                                    class: "border-b border-base-800 hover:bg-base-1000 cursor-pointer",
                                                    onclick: move |_| {
                                                        let cur = expanded.read().clone();
                                                        if cur.as_deref() == Some(event_id_for_toggle.as_str()) {
                                                            expanded.set(None);
                                                            replication_snapshot.set(None);
                                                        } else {
                                                            expanded.set(Some(event_id_for_toggle.clone()));
                                                            replication_snapshot.set(None);
                                                        }
                                                    },
                                                    td { class: "px-3 py-2 text-base-500 text-sm w-6", if is_open { "▾" } else { "▸" } }
                                                    td { class: "px-3 py-2 text-sm text-base-500",
                                                        title: "{format_date(Some(ev.created_at_unix_ms))}",
                                                        "{format_relative_time(Some(ev.created_at_unix_ms))}"
                                                    }
                                                    td { class: "px-3 py-2 text-sm font-mono text-accent-100", "{ev.bucket}" }
                                                    td { class: "px-3 py-2 text-sm font-mono text-base-300", title: "{ev.origin_node_id}", "{origin_short}:{ev.origin_epoch}" }
                                                    td { class: "px-3 py-2 text-sm font-mono text-right text-base-300", "{ev.origin_seq}" }
                                                    td { class: "px-3 py-2", {event_type_badge(&ev.r#type)} }
                                                    td { class: "px-3 py-2 text-sm font-mono text-base-300", "{ev.account}" }
                                                    td {
                                                        class: if ev.amount < 0 { "px-3 py-2 text-right text-sm font-mono text-[#f87171]" } else { "px-3 py-2 text-right text-sm font-mono text-accent-100" },
                                                        "{format_signed_amount(ev.amount)}"
                                                    }
                                                    td { class: "px-3 py-2 text-sm text-base-400 max-w-[24ch] overflow-hidden text-ellipsis whitespace-nowrap",
                                                        title: "{note_txt}",
                                                        "{note_txt}"
                                                    }
                                                    td { class: "px-3 py-2 text-center",
                                                        span {
                                                            class: "inline-block w-2 h-2 rounded-full {dot_color}",
                                                            title: "{dot_title}",
                                                        }
                                                    }
                                                }
                                                if is_open {
                                                    tr { class: "border-b border-base-800",
                                                        td { colspan: 10, class: "p-0",
                                                            EventDetail {
                                                                event: ev.clone(),
                                                                head: head,
                                                                max_known: max_known,
                                                                replication: replication_snapshot.read().clone(),
                                                                replication_loading: *replication_loading.read(),
                                                                on_load_replication: move |_| {
                                                                    if replication_snapshot.read().is_some() || *replication_loading.read() {
                                                                        return;
                                                                    }
                                                                    let f = applied.read().clone();
                                                                    let p = *page.read();
                                                                    replication_loading.set(true);
                                                                    spawn(async move {
                                                                        let res = scope.fetch(&f, p, PAGE_SIZE, true).await.ok();
                                                                        if let Some(r) = res {
                                                                            replication_snapshot.set(r.replication);
                                                                        }
                                                                        replication_loading.set(false);
                                                                    });
                                                                },
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
                        {
                            let total_pages = (d.total as usize).div_ceil(PAGE_SIZE);
                            let total_pages = total_pages.max(1);
                            let p = *page.read();
                            rsx! {
                                Pagination {
                                    total: d.total as usize, page: p, total_pages: total_pages, label: "events".to_string(),
                                    loading: *list_loading.read(),
                                    on_prev: move |_| { list_loading.set(true); expanded.set(None); page.set(p - 1); },
                                    on_next: move |_| { list_loading.set(true); expanded.set(None); page.set(p + 1); },
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
fn FilterInput(
    label: String,
    value: String,
    #[props(default = String::new())] placeholder: String,
    on_input: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: "grid gap-1",
            label { class: "text-[11px] uppercase tracking-widest text-base-500 font-mono", "{label}" }
            input {
                class: "w-full px-2 py-1 rounded border border-base-800 bg-base-1000 text-sm font-mono text-fg",
                value: "{value}",
                placeholder: "{placeholder}",
                oninput: move |evt| on_input.call(evt.value()),
            }
        }
    }
}

#[component]
fn FilterInputDateTime(label: String, value: String, on_input: EventHandler<String>) -> Element {
    rsx! {
        div { class: "grid gap-1",
            label { class: "text-[11px] uppercase tracking-widest text-base-500 font-mono", "{label}" }
            input {
                r#type: "datetime-local",
                class: "w-full px-2 py-1 rounded border border-base-800 bg-base-1000 text-sm font-mono text-fg",
                value: "{value}",
                oninput: move |evt| on_input.call(evt.value()),
            }
        }
    }
}

#[component]
fn EventDetail(
    event: AdminEvent,
    head: u64,
    max_known: u64,
    replication: Option<ReplicationSnapshot>,
    replication_loading: bool,
    on_load_replication: EventHandler<()>,
) -> Element {
    let json = serde_json::to_string_pretty(&serde_json::json!({
        "event_id": event.event_id,
        "origin_node_id": event.origin_node_id,
        "origin_epoch": event.origin_epoch,
        "origin_seq": event.origin_seq,
        "created_at_unix_ms": event.created_at_unix_ms,
        "type": event.r#type,
        "bucket": event.bucket,
        "account": event.account,
        "amount": event.amount,
        "note": event.note,
        "idempotency_nonce": event.idempotency_nonce,
        "void_ref": event.void_ref,
        "hold_amount": event.hold_amount,
        "hold_expires_at_unix_ms": event.hold_expires_at_unix_ms,
    }))
    .unwrap_or_default();

    let origin_seq = event.origin_seq;

    rsx! {
        div { class: "bg-base-1000 p-4 grid gap-4",
            div { class: "grid md:grid-cols-2 gap-4",
                div {
                    div { class: "text-[11px] uppercase tracking-widest text-base-500 font-mono mb-1", "Event payload" }
                    pre { class: "p-3 rounded border border-base-800 bg-base-900 text-xs font-mono text-base-300 whitespace-pre overflow-auto max-h-[340px]",
                        "{json}"
                    }
                }
                div {
                    div { class: "text-[11px] uppercase tracking-widest text-base-500 font-mono mb-1", "Replication status (this node)" }
                    div { class: "p-3 rounded border border-base-800 bg-base-900 text-xs font-mono text-base-300 grid gap-1",
                        div { "origin_seq: ", span { class: "text-fg", "{origin_seq}" } }
                        div { "contiguous head: ", span { class: "text-fg", "{head}" } }
                        div { "max_known_seq: ", span { class: "text-fg", "{max_known}" } }
                        {
                            let (tone, label) = status_label(origin_seq, head, max_known);
                            rsx! { div { class: "mt-1", Badge { text: label.to_string(), tone: tone } } }
                        }
                    }
                    div { class: "text-[11px] uppercase tracking-widest text-base-500 font-mono mt-3 mb-1", "Per-node replication" }
                    match &replication {
                        Some(snap) => rsx! {
                            div { class: "rounded border border-base-800 bg-base-900 overflow-hidden",
                                table { class: "w-full border-collapse",
                                    thead { class: "bg-base-1000",
                                        tr {
                                            th { class: "px-3 py-2 text-left text-base-500 uppercase tracking-widest text-[10px] font-mono", "Node" }
                                            th { class: "px-3 py-2 text-right text-base-500 uppercase tracking-widest text-[10px] font-mono", "Head" }
                                            th { class: "px-3 py-2 text-center text-base-500 uppercase tracking-widest text-[10px] font-mono", "Status" }
                                        }
                                    }
                                    tbody {
                                        if snap.per_node.is_empty() {
                                            tr { td { colspan: 3, class: "px-3 py-2 text-center text-base-500 text-xs", "No peers." } }
                                        }
                                        for (label, entry) in snap.per_node.iter() {
                                            {
                                                let key = format!("{}\t{}:{}", event.bucket, event.origin_node_id, event.origin_epoch);
                                                let node_head = entry.heads.get(&key).copied().unwrap_or(0);
                                                let node_max = entry.max_known_seqs.get(&key).copied().unwrap_or(node_head);
                                                let (tone, status) = status_label(origin_seq, node_head, node_max);
                                                let err = entry.error.clone();
                                                rsx! {
                                                    tr { key: "{label}", class: "border-t border-base-800",
                                                        td { class: "px-3 py-2 text-xs font-mono text-fg", "{label}" }
                                                        td { class: "px-3 py-2 text-xs font-mono text-right text-base-300", "{node_head}" }
                                                        td { class: "px-3 py-2 text-center",
                                                            if let Some(e) = err {
                                                                span { class: "text-[#f87171] text-xs", title: "{e}", "err" }
                                                            } else {
                                                                Badge { text: status.to_string(), tone: tone }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        None => rsx! {
                            div { class: "p-3 rounded border border-dashed border-base-800 bg-base-900 text-xs font-mono text-base-500 flex items-center gap-3",
                                if replication_loading {
                                    span { class: "inline-block w-3 h-3 border-2 border-base-700 border-t-accent-100 rounded-full animate-spin" }
                                    "Fetching per-node snapshot…"
                                } else {
                                    button {
                                        class: "px-2 py-0.5 rounded border border-accent-100/40 text-accent-100 hover:bg-accent-100/10 transition-colors",
                                        onclick: move |evt| { evt.stop_propagation(); on_load_replication.call(()); },
                                        "Load replication matrix"
                                    }
                                    span { "Fans out a State RPC to every peer." }
                                }
                            }
                        },
                    }
                }
            }
        }
    }
}

fn short_origin(origin: &str) -> String {
    let len = origin.len().min(8);
    origin[..len].to_string()
}

fn replication_dot(seq: u64, head: u64, max_known: u64) -> (&'static str, &'static str) {
    if seq <= head {
        ("bg-accent-100", "landed — contiguous to this event")
    } else if seq <= max_known {
        ("bg-accent-200", "pending — waiting for an earlier seq")
    } else {
        ("bg-base-600", "unknown — beyond this node's max_known")
    }
}

fn status_label(seq: u64, head: u64, max_known: u64) -> (BadgeTone, &'static str) {
    if seq <= head {
        (BadgeTone::Success, "landed")
    } else if seq <= max_known {
        (BadgeTone::Warning, "pending")
    } else {
        (BadgeTone::Neutral, "unknown")
    }
}

/// Parse a `<input type="datetime-local">` value (e.g. "2026-04-22T14:30")
/// into unix ms. Assumes local timezone — good enough for admin filters.
fn parse_datetime_local_to_ms(value: &str) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    let js_date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(value));
    let ms = js_date.get_time();
    if ms.is_nan() { None } else { Some(ms as u64) }
}
