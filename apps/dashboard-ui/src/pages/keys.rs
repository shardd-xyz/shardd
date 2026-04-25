use crate::api;
use crate::api::developer::{CreateKeyRequest, CreateScopeRequest};
use crate::components::badge::{Badge, BadgeTone};
use crate::components::button::{Btn, BtnSize, BtnVariant};
use crate::components::time::*;
use crate::router::Route;
use crate::state::{use_flash_key, use_notice};
use crate::types::{ApiKey, ApiKeyScope, FlashKey, Notice, NoticeTone};
use chrono::{Duration, Utc};
use dioxus::prelude::*;

#[component]
pub fn Keys() -> Element {
    let mut status_filter = use_signal(|| "active".to_string());

    let mut keys = use_resource(move || async move { api::developer::list_keys().await.ok() });

    let all_keys = keys.read();
    let all_keys = all_keys.as_ref().and_then(|k| k.as_ref());
    let filter = status_filter.read().clone();
    let filtered: Vec<_> = all_keys
        .map(|keys| {
            keys.iter()
                .filter(|k| {
                    let status = if k.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "active"
                    };
                    filter == "all" || status == filter
                })
                .collect()
        })
        .unwrap_or_default();
    let total_count = all_keys.map(|k| k.len()).unwrap_or(0);

    rsx! {
        div { class: "grid gap-6 w-full",
        section { class: "flex flex-wrap justify-between items-start gap-4",
            div { class: "grid gap-1",
                span { class: "text-accent-100 font-mono text-[12px] uppercase tracking-[-0.015rem]", "Developer" }
                h1 { class: "text-[32px] font-mono font-normal leading-[100%] tracking-[-0.04em] text-fg", "Keys" }
            }
            div { class: "flex gap-2.5",
                Link { to: Route::Dashboard, class: "px-3 py-1.5 rounded-sm font-mono text-[12px] uppercase tracking-[-0.015rem] border border-base-700 text-base-400 hover:text-fg transition-colors duration-150 no-underline", "Back home" }
            }
        }

        FlashKeyBanner {}

        CreateKeyWizard {
            active_count: all_keys.map(|k| k.iter().filter(|k| k.revoked_at.is_none()).count()).unwrap_or(0),
            on_created: move |_| keys.restart(),
        }

        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
            div { class: "flex justify-between items-start",
                h2 { class: "text-[16px] font-mono font-normal text-fg", "Keys" }
                Badge { text: if filtered.len() != total_count { format!("{} of {}", filtered.len(), total_count) } else { total_count.to_string() }, tone: BadgeTone::Neutral }
            }

            nav { class: "flex gap-2",
                for (value, label) in [("active", "Active"), ("revoked", "Revoked"), ("all", "All")] {
                    button {
                        class: if *status_filter.read() == value {
                            "px-2 py-0.5 rounded font-mono text-[11px] uppercase tracking-[-0.01rem] border border-base-800 bg-base-1000 text-fg transition-colors"
                        } else {
                            "px-2 py-0.5 rounded font-mono text-[11px] uppercase tracking-[-0.01rem] border border-transparent text-base-500 hover:text-fg transition-colors"
                        },
                        onclick: move |_| status_filter.set(value.to_string()),
                        "{label}"
                    }
                }
            }

            if filtered.is_empty() {
                if total_count == 0 {
                    crate::components::empty_state::EmptyState {
                        title: "No API keys yet".to_string(),
                        body: "Create your first key, then use it to write events. You'll see the raw key once \u{2014} copy it somewhere safe.".to_string(),
                        cta: None,
                        external_cta: Some(("Read the quickstart".to_string(), "https://shardd.xyz/guide/quickstart".to_string())),
                    }
                } else {
                    div { class: "py-7 text-center text-base-500 font-mono text-[14px]", "No {filter} API keys found." }
                }
            } else {
                div { class: "grid gap-3",
                    for key in &filtered {
                        KeyCard { api_key: (*key).clone(), on_change: move |_| keys.restart() }
                    }
                }
            }
        }
        }
    }
}

#[component]
fn KeyCard(api_key: ApiKey, on_change: EventHandler<()>) -> Element {
    let key = &api_key;
    let status = if key.revoked_at.is_some() {
        "revoked"
    } else {
        "active"
    };
    let status_tone = if status == "active" {
        BadgeTone::Success
    } else {
        BadgeTone::Warning
    };
    let is_active = status == "active";
    let key_id = key.id.clone();
    let key_id2 = key.id.clone();
    let key_id3 = key.id.clone();
    let key_name = key.name.clone();
    let mut notice = use_notice();

    let mut scopes: Signal<Option<Vec<ApiKeyScope>>> = use_signal(|| None);
    let mut scopes_loading = use_signal(|| false);
    let mut scope_match = use_signal(|| "all".to_string());
    let mut scope_bucket = use_signal(String::new);
    let mut scope_read = use_signal(|| true);
    let mut scope_write = use_signal(|| false);

    let load_scopes = move |_| {
        let kid = key_id.clone();
        scopes_loading.set(true);
        spawn(async move {
            match api::developer::list_key_scopes(&kid).await {
                Ok(s) => scopes.set(Some(s)),
                Err(_) => scopes.set(Some(vec![])),
            }
            scopes_loading.set(false);
        });
    };

    let on_revoke = {
        let kid = key_id2.clone();
        let kname = key_name.clone();
        move |_| {
            let kid = kid.clone();
            let kname = kname.clone();
            spawn(async move {
                if let Err(e) = api::developer::revoke_key(&kid).await {
                    notice.set(Some(Notice::new(
                        NoticeTone::Danger,
                        "Revoke failed",
                        e.friendly().1,
                    )));
                } else {
                    notice.set(Some(Notice::new(
                        NoticeTone::Success,
                        "Key revoked",
                        format!("{kname} has been revoked."),
                    )));
                    on_change.call(());
                }
            });
        }
    };

    let on_add_scope = {
        let kid = key_id3.clone();
        move |evt: FormEvent| {
            evt.prevent_default();
            let kid = kid.clone();
            let mt = scope_match.read().clone();
            let bkt = scope_bucket.read().clone();
            let cr = *scope_read.read();
            let cw = *scope_write.read();
            spawn(async move {
                let req = CreateScopeRequest {
                    resource_type: Some("bucket".to_string()),
                    match_type: mt,
                    resource_value: if scope_bucket.read().is_empty() {
                        None
                    } else {
                        Some(bkt)
                    },
                    can_read: cr,
                    can_write: cw,
                };
                match api::developer::create_scope(&kid, &req).await {
                    Ok(new_scope) => {
                        let mut current = scopes.read().clone().unwrap_or_default();
                        current.push(new_scope);
                        scopes.set(Some(current));
                        scope_bucket.set(String::new());
                        notice.set(Some(Notice::new(
                            NoticeTone::Success,
                            "Scope added",
                            "New scope attached to key.",
                        )));
                    }
                    Err(e) => {
                        notice.set(Some(Notice::new(
                            NoticeTone::Danger,
                            "Scope creation failed",
                            e.friendly().1,
                        )));
                    }
                }
            });
        }
    };

    rsx! {
        article { class: "rounded-lg border border-base-800 bg-base-900 p-3.5 grid gap-3",
            div { class: "flex flex-wrap items-center gap-2.5",
                strong { class: "font-mono text-[14px] text-fg", "{key.name}" }
                Badge { text: status.to_string(), tone: status_tone }
                span { class: "px-1.5 py-0.5 rounded border border-base-800 bg-base-1000 text-base-400 font-mono text-[11px]", "{key.key_prefix}" }
                span { class: "text-base-500 font-mono text-[11px] whitespace-nowrap",
                    title: "{format_date_str(key.last_used_at.as_deref())}",
                    "Used {format_relative_time_str(key.last_used_at.as_deref())}"
                }
                if let Some(exp) = &key.expires_at {
                    span { class: "text-base-500 font-mono text-[11px] whitespace-nowrap",
                        title: "{format_date_str(Some(exp))}",
                        "Expires {format_relative_time_str(Some(exp))}"
                    }
                }
                div { class: "flex gap-2 ml-auto",
                    if is_active {
                        Btn { variant: BtnVariant::Ghost, size: BtnSize::Sm, "Rotate" }
                        Btn { variant: BtnVariant::Danger, size: BtnSize::Sm, onclick: on_revoke, "Revoke" }
                    }
                }
            }

            details {
                class: "cursor-pointer",
                ontoggle: load_scopes,
                summary { class: "font-mono text-[12px] text-base-500 uppercase tracking-[-0.015rem]",
                    "Scopes"
                    if let Some(s) = scopes.read().as_ref() {
                        span { class: "ml-2 text-base-600", " · {s.len()} total" }
                    }
                }
                div { class: "mt-3 grid gap-3",
                    if *scopes_loading.read() {
                        div { class: "text-base-500 font-mono text-[14px]", "Loading scopes…" }
                    } else if let Some(scope_list) = scopes.read().as_ref() {
                        if scope_list.is_empty() {
                            div { class: "text-base-500 font-mono text-[14px]", "No scopes attached." }
                        } else {
                            div { class: "grid gap-2",
                                for scope in scope_list {
                                    {
                                        let scope_id = scope.id.clone();
                                        let on_remove = move |_| {
                                            let sid = scope_id.clone();
                                            spawn(async move {
                                                match api::developer::delete_scope(&sid).await {
                                                    Ok(_) => {
                                                        let mut current = scopes.read().clone().unwrap_or_default();
                                                        current.retain(|s| s.id != sid);
                                                        scopes.set(Some(current));
                                                        notice.set(Some(Notice::new(NoticeTone::Success, "Scope removed", "Scope detached from key.")));
                                                    }
                                                    Err(e) => {
                                                        notice.set(Some(Notice::new(NoticeTone::Danger, "Scope removal failed", e.friendly().1)));
                                                    }
                                                }
                                            });
                                        };
                                        {
                                            let is_control = scope.resource_type == "control";
                                            let perms = format!(
                                                "{} · {}",
                                                if scope.can_read { "read" } else { "no-read" },
                                                if scope.can_write { "write" } else { "no-write" },
                                            );
                                            let bucket_label = if scope.match_type == "all" {
                                                "all buckets".to_string()
                                            } else {
                                                scope
                                                    .resource_value
                                                    .clone()
                                                    .unwrap_or_else(|| "all buckets".to_string())
                                            };
                                            rsx! {
                                                div { class: "flex items-center gap-2.5 p-2.5 rounded-lg border border-base-800 bg-base-1000",
                                                    if is_control {
                                                        Badge { text: "dashboard control".to_string(), tone: BadgeTone::Warning }
                                                        span { class: "text-base-500 font-mono text-[11px]",
                                                            "manage keys, buckets, profile, billing"
                                                        }
                                                    } else {
                                                        Badge { text: scope.match_type.clone(), tone: BadgeTone::Primary }
                                                        span { class: "px-1.5 py-0.5 rounded border border-base-800 bg-base-900 text-base-300 font-mono text-[11px]",
                                                            "{bucket_label}"
                                                        }
                                                    }
                                                    span { class: "text-base-500 font-mono text-[11px]", "{perms}" }
                                                    if is_active {
                                                        div { class: "ml-auto",
                                                            Btn { variant: BtnVariant::Danger, size: BtnSize::Sm, onclick: on_remove, "Remove" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        if is_active {
                            form { class: "grid gap-3 mt-2 pt-3 border-t border-base-800",
                                onsubmit: on_add_scope,
                                div { class: "flex gap-3 flex-wrap",
                                    div { class: "grid gap-1",
                                        label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Match" }
                                        select {
                                            value: "{scope_match}",
                                            onchange: move |e| scope_match.set(e.value()),
                                            option { value: "all", "all buckets" }
                                            option { value: "exact", "exact bucket" }
                                            option { value: "prefix", "bucket prefix" }
                                        }
                                    }
                                    div { class: "grid gap-1 flex-1",
                                        label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Bucket" }
                                        input { r#type: "text", placeholder: "orders or orders/", value: "{scope_bucket}", oninput: move |e| scope_bucket.set(e.value()) }
                                    }
                                }
                                div { class: "flex items-center gap-4",
                                    label { class: "flex items-center gap-2 text-base-400 font-mono text-[12px]",
                                        input { r#type: "checkbox", checked: *scope_read.read(), onchange: move |e: FormEvent| scope_read.set(e.value() == "true") }
                                        "Read"
                                    }
                                    label { class: "flex items-center gap-2 text-base-400 font-mono text-[12px]",
                                        input { r#type: "checkbox", checked: *scope_write.read(), onchange: move |e: FormEvent| scope_write.set(e.value() == "true") }
                                        "Write"
                                    }
                                    Btn { r#type: "submit".to_string(), variant: BtnVariant::Primary, size: BtnSize::Sm, "Add scope" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn FlashKeyBanner() -> Element {
    let mut flash = use_flash_key();

    let flash_read = flash.read();
    let Some(fk) = flash_read.as_ref() else {
        return rsx! {};
    };
    let label = fk.label.clone();
    let raw_key = fk.raw_key.clone();
    drop(flash_read);

    let clear_banner = move |_| {
        crate::state::save_flash_key_to_session(None);
        flash.set(None);
    };
    let on_copy = move |_| {
        // Copying the key is the user's signal that they've captured it. Clear
        // both in-memory and sessionStorage so the banner doesn't reappear on
        // next navigation.
        crate::state::save_flash_key_to_session(None);
        flash.set(None);
    };

    rsx! {
        section { class: "rounded-lg border border-dashed border-accent-100 bg-base-900 p-6 grid gap-3",
            strong { class: "text-fg font-mono text-[14px]", "API key created: {label}" }
            p { class: "text-base-400 font-mono text-[14px]", "This key will not be shown again \u{2014} copy it somewhere safe now." }
            div { class: "p-3 rounded-lg bg-base-1000 border border-base-800 font-mono text-[14px] text-accent-100 break-all", "{raw_key}" }
            div { class: "flex items-center gap-4",
                crate::components::copy_button::CopyButton {
                    value: raw_key.clone(),
                    label: Some("Copy key".to_string()),
                    on_copy: EventHandler::new(on_copy),
                }
                button {
                    class: "text-base-500 hover:text-fg font-mono text-[12px] uppercase tracking-[-0.015rem] bg-transparent border-0 w-fit transition-colors duration-150",
                    onclick: clear_banner,
                    "Dismiss"
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum WizardStep {
    Name,
    Scopes,
    Review,
}

#[derive(Clone, Copy, PartialEq)]
enum ExpiryPreset {
    Never,
    Days30,
    Days90,
    Year1,
}

impl ExpiryPreset {
    fn label(self) -> &'static str {
        match self {
            Self::Never => "Never",
            Self::Days30 => "30 days",
            Self::Days90 => "90 days",
            Self::Year1 => "1 year",
        }
    }
    fn code(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Days30 => "30d",
            Self::Days90 => "90d",
            Self::Year1 => "1y",
        }
    }
    fn parse(value: &str) -> Self {
        match value {
            "30d" => Self::Days30,
            "90d" => Self::Days90,
            "1y" => Self::Year1,
            _ => Self::Never,
        }
    }
    fn to_expires_at(self) -> Option<String> {
        let delta = match self {
            Self::Never => return None,
            Self::Days30 => Duration::days(30),
            Self::Days90 => Duration::days(90),
            Self::Year1 => Duration::days(365),
        };
        Some((Utc::now() + delta).to_rfc3339())
    }
}

#[derive(Clone, PartialEq)]
struct DraftScope {
    match_type: String,
    bucket: String,
    can_read: bool,
    can_write: bool,
}

impl Default for DraftScope {
    fn default() -> Self {
        // First scope the user adds should be immediately usable. Read-only
        // wasn't — devs would create a key, try to write, get a 403, and
        // bounce back to edit. Default to read+write across all buckets.
        Self {
            match_type: "all".to_string(),
            bucket: String::new(),
            can_read: true,
            can_write: true,
        }
    }
}

#[component]
fn CreateKeyWizard(active_count: usize, on_created: EventHandler<()>) -> Element {
    let mut step = use_signal(|| WizardStep::Name);
    let mut name = use_signal(String::new);
    let mut expiry = use_signal(|| ExpiryPreset::Never);
    // Seed the wizard with one permissive scope so the first key is usable
    // without knob-fiddling. The user can still edit or add more in step 2.
    let mut scopes: Signal<Vec<DraftScope>> = use_signal(|| vec![DraftScope::default()]);
    // Off by default — most keys are programmatic data-plane credentials
    // and shouldn't be able to manage other keys, archive buckets,
    // export data, etc. Devs who want a CLI/admin-style key tick this
    // explicitly.
    let mut allow_control_plane = use_signal(|| false);
    let mut submitting = use_signal(|| false);
    let mut notice = use_notice();
    let mut flash = use_flash_key();

    let validate_scopes = move || -> Result<(), (String, String)> {
        for (i, s) in scopes.read().iter().enumerate() {
            if !s.can_read && !s.can_write {
                return Err((
                    format!("Scope #{} needs a permission", i + 1),
                    "Enable read, write, or both.".to_string(),
                ));
            }
            if s.match_type != "all" && s.bucket.trim().is_empty() {
                return Err((
                    format!("Scope #{} needs a bucket", i + 1),
                    "Exact or prefix scopes must target a bucket.".to_string(),
                ));
            }
        }
        Ok(())
    };

    let mut submit = move || {
        let n = name.read().trim().to_string();
        if n.is_empty() {
            step.set(WizardStep::Name);
            return;
        }
        if let Err((title, message)) = validate_scopes() {
            notice.set(Some(Notice::new(NoticeTone::Danger, title, message)));
            step.set(WizardStep::Scopes);
            return;
        }
        let mut out_scopes: Vec<CreateScopeRequest> = scopes
            .read()
            .iter()
            .map(|s| CreateScopeRequest {
                resource_type: Some("bucket".to_string()),
                match_type: s.match_type.clone(),
                resource_value: if s.match_type == "all" || s.bucket.trim().is_empty() {
                    None
                } else {
                    Some(s.bucket.trim().to_string())
                },
                can_read: s.can_read,
                can_write: s.can_write,
            })
            .collect();
        if *allow_control_plane.read() {
            out_scopes.push(CreateScopeRequest {
                resource_type: Some("control".to_string()),
                match_type: "all".to_string(),
                resource_value: None,
                can_read: true,
                can_write: true,
            });
        }
        let req = CreateKeyRequest {
            name: n.clone(),
            expires_at: expiry.read().to_expires_at(),
            scopes: out_scopes,
        };
        submitting.set(true);
        spawn(async move {
            match api::developer::create_key(&req).await {
                Ok(issued) => {
                    let fk = FlashKey {
                        label: n,
                        raw_key: issued.raw_key,
                    };
                    // Persist so a reload/nav-away doesn't lose the one-shot
                    // raw key. Cleared when the user copies it (see FlashKeyBanner).
                    crate::state::save_flash_key_to_session(Some(&fk));
                    flash.set(Some(fk));
                    name.set(String::new());
                    scopes.set(vec![DraftScope::default()]);
                    allow_control_plane.set(false);
                    expiry.set(ExpiryPreset::Never);
                    step.set(WizardStep::Name);
                    on_created.call(());
                }
                Err(e) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Danger,
                        "Key creation failed",
                        e.friendly().1,
                    )));
                }
            }
            submitting.set(false);
        });
    };

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
            div { class: "flex justify-between items-start",
                h2 { class: "text-[16px] font-mono font-normal text-fg", "Create API key" }
                Badge {
                    text: format!("{active_count} active"),
                    tone: if active_count > 0 { BadgeTone::Success } else { BadgeTone::Neutral },
                }
            }

            StepIndicator { current: *step.read() }

            match *step.read() {
                WizardStep::Name => rsx! {
                    div { class: "grid gap-3",
                        div { class: "flex gap-3 flex-wrap",
                            div { class: "flex-1 min-w-[220px] grid gap-1",
                                label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Key name" }
                                input {
                                    r#type: "text",
                                    placeholder: "production worker",
                                    value: "{name}",
                                    oninput: move |e| name.set(e.value()),
                                }
                            }
                            div { class: "w-[180px] grid gap-1",
                                label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Expires" }
                                select {
                                    value: "{expiry.read().code()}",
                                    onchange: move |e| expiry.set(ExpiryPreset::parse(&e.value())),
                                    option { value: "never", "Never" }
                                    option { value: "30d", "30 days" }
                                    option { value: "90d", "90 days" }
                                    option { value: "1y", "1 year" }
                                }
                            }
                        }
                        div { class: "flex justify-end",
                            Btn {
                                variant: BtnVariant::Primary,
                                size: BtnSize::Default,
                                disabled: name.read().trim().is_empty(),
                                onclick: move |_| step.set(WizardStep::Scopes),
                                "Next: scopes"
                            }
                        }
                    }
                },
                WizardStep::Scopes => rsx! {
                    div { class: "grid gap-3",
                        p { class: "font-mono text-[12px] text-base-500 leading-[140%]",
                            "Scopes are optional. A key with no scopes can be used for authentication but cannot access any buckets until you add one."
                        }
                        {
                            let current = scopes.read().clone();
                            if current.is_empty() {
                                rsx! {
                                    div { class: "py-4 text-center text-base-500 font-mono text-[13px] border border-dashed border-base-800 rounded",
                                        "No scopes yet."
                                    }
                                }
                            } else {
                                rsx! {
                                    div { class: "grid gap-2",
                                        for (idx, draft) in current.iter().enumerate() {
                                            DraftScopeRow {
                                                key: "{idx}",
                                                index: idx,
                                                draft: draft.clone(),
                                                on_change: move |updated: DraftScope| {
                                                    let mut v = scopes.read().clone();
                                                    if let Some(slot) = v.get_mut(idx) { *slot = updated; }
                                                    scopes.set(v);
                                                },
                                                on_remove: move |_| {
                                                    let mut v = scopes.read().clone();
                                                    if idx < v.len() { v.remove(idx); }
                                                    scopes.set(v);
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "flex flex-wrap items-center gap-2",
                            Btn {
                                variant: BtnVariant::Ghost,
                                size: BtnSize::Sm,
                                onclick: move |_| {
                                    let mut v = scopes.read().clone();
                                    v.push(DraftScope::default());
                                    scopes.set(v);
                                },
                                "+ Add scope"
                            }
                        }
                        {
                            let allow = *allow_control_plane.read();
                            rsx! {
                                label {
                                    class: "rounded-lg border border-dashed border-base-700 bg-base-1000 p-4 grid gap-1 cursor-pointer",
                                    div { class: "flex items-center gap-2",
                                        input {
                                            r#type: "checkbox",
                                            checked: allow,
                                            onchange: move |evt| {
                                                allow_control_plane.set(evt.checked());
                                            },
                                        }
                                        span { class: "font-mono text-[13px] text-fg",
                                            "Allow dashboard control"
                                        }
                                        Badge {
                                            text: (if allow { "Granted" } else { "Off" }).to_string(),
                                            tone: if allow { BadgeTone::Warning } else { BadgeTone::Neutral },
                                        }
                                    }
                                    p { class: "font-mono text-[12px] text-base-500 leading-[160%] m-0",
                                        "Off by default. Enable only for keys that need to manage other keys, archive/purge buckets, or update the account profile (this is what the "
                                        code { class: "text-accent-100", "shardd" }
                                        " CLI uses). Data-plane writes don't need this."
                                    }
                                }
                            }
                        }
                        div { class: "flex justify-between pt-2",
                            Btn {
                                variant: BtnVariant::Ghost,
                                size: BtnSize::Default,
                                onclick: move |_| step.set(WizardStep::Name),
                                "Back"
                            }
                            Btn {
                                variant: BtnVariant::Primary,
                                size: BtnSize::Default,
                                onclick: move |_| {
                                    if let Err((title, message)) = validate_scopes() {
                                        notice.set(Some(Notice::new(NoticeTone::Danger, title, message)));
                                        return;
                                    }
                                    step.set(WizardStep::Review);
                                },
                                "Next: review"
                            }
                        }
                    }
                },
                WizardStep::Review => rsx! {
                    {
                        let reviewed_name = name.read().clone();
                        let reviewed_expiry = expiry.read().label();
                        let reviewed_scopes = scopes.read().clone();
                        let busy = *submitting.read();
                        rsx! {
                            div { class: "grid gap-3",
                                div { class: "grid gap-1",
                                    span { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Name" }
                                    span { class: "font-mono text-[14px] text-fg", "{reviewed_name}" }
                                }
                                div { class: "grid gap-1",
                                    span { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Expires" }
                                    span { class: "font-mono text-[14px] text-fg", "{reviewed_expiry}" }
                                }
                                div { class: "grid gap-1",
                                    span { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500",
                                        "Scopes ({reviewed_scopes.len()})"
                                    }
                                    if reviewed_scopes.is_empty() {
                                        span { class: "font-mono text-[14px] text-base-500", "none" }
                                    } else {
                                        div { class: "grid gap-2",
                                            for scope in reviewed_scopes.iter() {
                                                ReviewScopeRow { draft: scope.clone() }
                                            }
                                        }
                                    }
                                }
                                div { class: "grid gap-1",
                                    span { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500",
                                        "Dashboard control"
                                    }
                                    span { class: "font-mono text-[14px] text-fg",
                                        if *allow_control_plane.read() { "granted (manage keys, buckets, profile, billing)" } else { "off (data plane only)" }
                                    }
                                }
                                div { class: "flex justify-between pt-2",
                                    Btn {
                                        variant: BtnVariant::Ghost,
                                        size: BtnSize::Default,
                                        disabled: busy,
                                        onclick: move |_| step.set(WizardStep::Scopes),
                                        "Back"
                                    }
                                    Btn {
                                        variant: BtnVariant::Primary,
                                        size: BtnSize::Default,
                                        disabled: busy,
                                        onclick: move |_| submit(),
                                        if busy { "Creating…" } else { "Create key" }
                                    }
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn StepIndicator(current: WizardStep) -> Element {
    let step_span = |num: &'static str, label: &'static str, active: bool| {
        let num_cls = if active {
            "text-accent-100 font-mono text-[12px] tracking-[-0.015rem]"
        } else {
            "text-base-600 font-mono text-[12px] tracking-[-0.015rem]"
        };
        let label_cls = if active {
            "text-fg font-mono text-[12px] uppercase tracking-[-0.015rem]"
        } else {
            "text-base-500 font-mono text-[12px] uppercase tracking-[-0.015rem]"
        };
        rsx! {
            span { class: "flex items-center gap-1.5",
                span { class: "{num_cls}", "{num}" }
                span { class: "{label_cls}", "{label}" }
            }
        }
    };
    let divider = || {
        rsx! { span { class: "text-base-700 font-mono text-[12px]", "—" } }
    };
    rsx! {
        div { class: "flex items-center gap-3 pb-2 border-b border-base-800",
            {step_span("01", "Name", current == WizardStep::Name)}
            {divider()}
            {step_span("02", "Scopes", current == WizardStep::Scopes)}
            {divider()}
            {step_span("03", "Review", current == WizardStep::Review)}
        }
    }
}

#[component]
fn DraftScopeRow(
    index: usize,
    draft: DraftScope,
    on_change: EventHandler<DraftScope>,
    on_remove: EventHandler<()>,
) -> Element {
    let badge_num = format!("{:02}", index + 1);
    rsx! {
        div { class: "p-3 rounded-lg border border-base-800 bg-base-1000 grid gap-3",
            div { class: "flex items-center justify-between",
                span { class: "font-mono text-[11px] uppercase tracking-[-0.015rem] text-accent-100", "Scope {badge_num}" }
                button {
                    class: "text-base-500 hover:text-[#f87171] font-mono text-[11px] uppercase tracking-[-0.015rem] bg-transparent border-0 transition-colors duration-150",
                    onclick: move |_| on_remove.call(()),
                    "Remove"
                }
            }
            div { class: "flex gap-3 flex-wrap",
                div { class: "grid gap-1",
                    label { class: "font-mono text-[11px] uppercase tracking-[-0.015rem] text-base-500", "Match" }
                    select {
                        value: "{draft.match_type}",
                        onchange: {
                            let d = draft.clone();
                            move |e: Event<FormData>| {
                                let mut next = d.clone();
                                next.match_type = e.value();
                                on_change.call(next);
                            }
                        },
                        option { value: "all", "all buckets" }
                        option { value: "exact", "exact bucket" }
                        option { value: "prefix", "bucket prefix" }
                    }
                }
                div { class: "grid gap-1 flex-1 min-w-[180px]",
                    label { class: "font-mono text-[11px] uppercase tracking-[-0.015rem] text-base-500", "Bucket" }
                    input {
                        r#type: "text",
                        placeholder: if draft.match_type == "all" { "(n/a for all buckets)" } else { "orders or orders/" },
                        value: "{draft.bucket}",
                        disabled: draft.match_type == "all",
                        oninput: {
                            let d = draft.clone();
                            move |e: Event<FormData>| {
                                let mut next = d.clone();
                                next.bucket = e.value();
                                on_change.call(next);
                            }
                        },
                    }
                }
            }
            div { class: "flex items-center gap-4",
                label { class: "flex items-center gap-2 text-base-400 font-mono text-[12px]",
                    input {
                        r#type: "checkbox",
                        checked: draft.can_read,
                        onchange: {
                            let d = draft.clone();
                            move |e: Event<FormData>| {
                                let mut next = d.clone();
                                next.can_read = e.value() == "true";
                                on_change.call(next);
                            }
                        },
                    }
                    "Read"
                }
                label { class: "flex items-center gap-2 text-base-400 font-mono text-[12px]",
                    input {
                        r#type: "checkbox",
                        checked: draft.can_write,
                        onchange: {
                            let d = draft.clone();
                            move |e: Event<FormData>| {
                                let mut next = d.clone();
                                next.can_write = e.value() == "true";
                                on_change.call(next);
                            }
                        },
                    }
                    "Write"
                }
            }
        }
    }
}

#[component]
fn ReviewScopeRow(draft: DraftScope) -> Element {
    let bucket_label = if draft.match_type == "all" {
        "all buckets".to_string()
    } else if draft.bucket.trim().is_empty() {
        "(no bucket)".to_string()
    } else {
        draft.bucket.clone()
    };
    let perms = format!(
        "{} · {}",
        if draft.can_read { "read" } else { "no-read" },
        if draft.can_write { "write" } else { "no-write" },
    );
    rsx! {
        div { class: "flex items-center gap-2.5 p-2.5 rounded-lg border border-base-800 bg-base-1000",
            Badge { text: draft.match_type.clone(), tone: BadgeTone::Primary }
            span { class: "px-1.5 py-0.5 rounded border border-base-800 bg-base-900 text-base-300 font-mono text-[11px]", "{bucket_label}" }
            span { class: "text-base-500 font-mono text-[11px]", "{perms}" }
        }
    }
}
