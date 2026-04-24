use crate::api;
use crate::components::badge::{Badge, BadgeTone};
use crate::components::button::{Btn, BtnSize, BtnVariant};
use crate::components::meta_row::{MetaRow, MetaRowCode};
use crate::components::time::*;
use crate::router::Route;
use crate::state::use_notice;
use crate::types::{Notice, NoticeTone};
use dioxus::prelude::*;

fn fmt_credits_exact(n: i64) -> String {
    let s = n.abs().to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    if n < 0 {
        result.push('-');
    }
    result.chars().rev().collect()
}

fn status_tone(status: &str) -> BadgeTone {
    match status {
        "active" => BadgeTone::Success,
        "manual" => BadgeTone::Primary,
        "canceled" | "none" => BadgeTone::Neutral,
        _ => BadgeTone::Warning,
    }
}

#[component]
pub fn AdminUser(user_id: String) -> Element {
    let data = use_resource({
        let uid = user_id.clone();
        move || {
            let uid = uid.clone();
            async move { api::admin::get_user(&uid).await.ok() }
        }
    });

    match &*data.read() {
        Some(Some(user)) => {
            let uid = user.id.clone();
            let is_deleted = user.deleted_at.is_some();
            rsx! {
                div { class: "grid gap-6 w-full",
                section { class: "flex flex-wrap justify-between items-start gap-4",
                    div { class: "grid gap-1",
                        span { class: "text-accent-100 text-xs uppercase tracking-widest", "Admin \u{b7} User" }
                        h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "{user.email}" }
                    }
                    div { class: "flex gap-2.5",
                        Link { to: Route::AdminUsers, class: "px-3.5 py-2 rounded-full border border-base-700 text-base-400 hover:text-fg transition no-underline text-sm", "All users" }
                    }
                }

                section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
                    div { class: "flex justify-between items-start",
                        h2 { class: "text-[16px] font-normal", "Summary" }
                        div { class: "flex gap-2 flex-wrap",
                            if is_deleted {
                                Badge { text: "deleted".to_string(), tone: BadgeTone::Danger }
                            } else if user.is_admin {
                                Badge { text: "admin".to_string(), tone: BadgeTone::Primary }
                            } else {
                                Badge { text: "developer".to_string(), tone: BadgeTone::Neutral }
                            }
                            if !is_deleted {
                                if user.is_frozen {
                                    Badge { text: "frozen".to_string(), tone: BadgeTone::Warning }
                                } else {
                                    Badge { text: "active".to_string(), tone: BadgeTone::Success }
                                }
                            }
                        }
                    }
                    div {
                        MetaRow { label: "Email".to_string(), value: user.email.clone() }
                        MetaRowCode { label: "User ID".to_string(), value: user.id.clone() }
                        MetaRow { label: "Last login".to_string(), value: format_relative_time_str(user.last_login_at.as_deref()) }
                        MetaRow { label: "Created".to_string(), value: format_date_str(user.created_at.as_deref()) }
                        if let Some(ts) = user.deleted_at.as_deref() {
                            MetaRow {
                                label: "Deleted".to_string(),
                                value: format!("{} ({})", format_date_str(Some(ts)), format_relative_time_str(Some(ts))),
                            }
                        }
                    }
                }

                if !is_deleted {
                    SubscriptionSection { user_id: uid.clone() }
                }

                section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
                    h2 { class: "text-[16px] font-normal", "Account actions" }
                    if is_deleted {
                        p { class: "text-base-500 font-mono text-[13px] leading-[140%]",
                            "This account has been deleted. Sign-in is blocked and all developer API keys are revoked. "
                            "Event history on the mesh stays resolvable via user ID."
                        }
                    } else if user.is_admin {
                        span { class: "text-base-500 text-sm", "Admin accounts cannot be frozen, impersonated, or deleted from this view." }
                    } else {
                        div { class: "flex gap-3",
                            button { class: "px-3 py-1.5 rounded-lg border border-base-700 text-base-400 hover:text-fg text-sm transition", "Impersonate" }
                            if user.is_frozen {
                                button { class: "px-3 py-1.5 rounded-lg border border-accent-100/30 text-accent-100 hover:text-accent-100 text-sm transition", "Unfreeze" }
                            } else {
                                button { class: "px-3 py-1.5 rounded-lg border border-accent-200/30 text-accent-200 hover:text-accent-200 text-sm transition", "Freeze" }
                            }
                            button { class: "px-3 py-1.5 rounded-lg border border-[#f87171]/30 text-[#f87171] hover:text-[#f87171] text-sm transition", "Delete" }
                        }
                    }
                }
                }
            }
        }
        Some(None) => rsx! {
            div { class: "text-base-500 text-center py-12", "User not found." }
        },
        None => rsx! {
            div { class: "text-base-500 text-center py-12", "Loading\u{2026}" }
        },
    }
}

#[component]
fn SubscriptionSection(user_id: String) -> Element {
    let mut notice = use_notice();

    let mut subscription = use_resource({
        let uid = user_id.clone();
        move || {
            let uid = uid.clone();
            async move { api::admin::get_subscription(&uid).await.ok() }
        }
    });

    let plans = use_resource(|| async { api::admin::list_plans().await.unwrap_or_default() });

    let mut plan_slug = use_signal(String::new);
    let mut credit_amount = use_signal(String::new);
    let mut credit_note = use_signal(String::new);
    let mut busy = use_signal(|| false);

    let on_assign = {
        let uid = user_id.clone();
        move |evt: FormEvent| {
            evt.prevent_default();
            let slug = plan_slug.read().clone();
            if slug.is_empty() {
                notice.set(Some(Notice::new(
                    NoticeTone::Danger,
                    "Pick a plan",
                    "Select a plan before assigning.",
                )));
                return;
            }
            let uid = uid.clone();
            busy.set(true);
            spawn(async move {
                match api::admin::set_plan(&uid, &slug).await {
                    Ok(_) => {
                        notice.set(Some(Notice::new(
                            NoticeTone::Success,
                            "Plan assigned",
                            format!("User is now on {slug}."),
                        )));
                        subscription.restart();
                    }
                    Err(e) => {
                        notice.set(Some(Notice::new(
                            NoticeTone::Danger,
                            "Plan assignment failed",
                            e.friendly().1,
                        )));
                    }
                }
                busy.set(false);
            });
        }
    };

    let on_grant = {
        let uid = user_id.clone();
        move |evt: FormEvent| {
            evt.prevent_default();
            let amt_str = credit_amount.read().clone();
            let note = credit_note.read().clone();
            let Ok(amount) = amt_str.parse::<i64>() else {
                notice.set(Some(Notice::new(
                    NoticeTone::Danger,
                    "Invalid amount",
                    "Amount must be a whole number (negative allowed).",
                )));
                return;
            };
            if amount == 0 {
                notice.set(Some(Notice::new(
                    NoticeTone::Danger,
                    "Invalid amount",
                    "Amount must be non-zero.",
                )));
                return;
            }
            if note.trim().is_empty() {
                notice.set(Some(Notice::new(
                    NoticeTone::Danger,
                    "Missing note",
                    "Add a short reason for this grant.",
                )));
                return;
            }
            let uid = uid.clone();
            busy.set(true);
            spawn(async move {
                match api::admin::grant_credits(&uid, amount, &note).await {
                    Ok(_) => {
                        notice.set(Some(Notice::new(
                            NoticeTone::Success,
                            "Credits granted",
                            format!("{amount:+} credits applied."),
                        )));
                        credit_amount.set(String::new());
                        credit_note.set(String::new());
                        subscription.restart();
                    }
                    Err(e) => {
                        notice.set(Some(Notice::new(
                            NoticeTone::Danger,
                            "Credit grant failed",
                            e.friendly().1,
                        )));
                    }
                }
                busy.set(false);
            });
        }
    };

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
            div { class: "flex justify-between items-start",
                h2 { class: "text-[16px] font-normal", "Subscription" }
            }
            match &*subscription.read() {
                Some(Some(sub)) => {
                    let rem_fmt = fmt_credits_exact(sub.credit_balance);
                    let total_fmt = fmt_credits_exact(sub.monthly_credits);
                    let status = sub.subscription_status.clone();
                    let tone = status_tone(&status);
                    let period = match (sub.period_start.as_deref(), sub.period_end.as_deref()) {
                        (Some(start), Some(end)) => format!("{} \u{2192} {}", format_date_str(Some(start)), format_date_str(Some(end))),
                        _ => String::new(),
                    };
                    rsx! {
                        div {
                            MetaRow { label: "Plan".to_string(), value: format!("{} ({})", sub.plan_name, sub.plan_slug) }
                            MetaRow { label: "Status".to_string(), value: status.clone() }
                            MetaRow { label: "Credits".to_string(), value: format!("{rem_fmt} / {total_fmt}") }
                            if !period.is_empty() {
                                MetaRow { label: "Period".to_string(), value: period }
                            }
                        }
                        div { class: "flex gap-2 flex-wrap",
                            Badge { text: status, tone }
                        }
                    }
                }
                Some(None) => rsx! { div { class: "text-base-500 font-mono text-[13px]", "Failed to load subscription." } },
                None => rsx! { div { class: "text-base-500 font-mono text-[13px]", "Loading\u{2026}" } },
            }

            form { class: "grid gap-3 pt-4 border-t border-base-800",
                onsubmit: on_assign,
                h3 { class: "text-[14px] font-mono font-normal text-fg", "Assign plan" }
                div { class: "flex gap-3 flex-wrap items-end",
                    div { class: "grid gap-1 flex-1 min-w-[200px]",
                        label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Plan" }
                        select {
                            value: "{plan_slug}",
                            onchange: move |e| plan_slug.set(e.value()),
                            option { value: "", "\u{2014}" }
                            match &*plans.read() {
                                Some(list) => rsx! {
                                    for p in list.iter() {
                                        option { value: "{p.slug}", "{p.name} ({p.slug})" }
                                    }
                                },
                                None => rsx! {},
                            }
                        }
                    }
                    Btn { r#type: "submit".to_string(), variant: BtnVariant::Primary, size: BtnSize::Default, disabled: *busy.read(), "Assign plan" }
                }
            }

            form { class: "grid gap-3 pt-4 border-t border-base-800",
                onsubmit: on_grant,
                h3 { class: "text-[14px] font-mono font-normal text-fg", "Grant credits" }
                div { class: "flex gap-3 flex-wrap items-end",
                    div { class: "grid gap-1 w-[180px]",
                        label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Amount" }
                        input {
                            r#type: "number", step: "1", placeholder: "100 or -50",
                            value: "{credit_amount}",
                            oninput: move |e| credit_amount.set(e.value()),
                        }
                    }
                    div { class: "grid gap-1 flex-1 min-w-[200px]",
                        label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Note" }
                        input {
                            r#type: "text", placeholder: "support comp / refund / etc.",
                            value: "{credit_note}",
                            oninput: move |e| credit_note.set(e.value()),
                        }
                    }
                    Btn { r#type: "submit".to_string(), variant: BtnVariant::Primary, size: BtnSize::Default, disabled: *busy.read(), "Grant credits" }
                }
            }
        }
    }
}
