use crate::api;
use crate::components::stat_card::StatCard;
use crate::router::Route;
use dioxus::prelude::*;

/// Short format for plan cards: 10K, 500K, 5M
fn fmt_credits_short(n: i64) -> String {
    if n.abs() >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        if m.fract() == 0.0 {
            format!("{}M", m as i64)
        } else {
            format!("{:.1}M", m)
        }
    } else if n.abs() >= 1_000 {
        let k = n as f64 / 1_000.0;
        if k.fract() == 0.0 {
            format!("{}K", k as i64)
        } else {
            format!("{:.1}K", k)
        }
    } else {
        n.to_string()
    }
}

/// Exact format with comma separators: 9,012 / 10,000
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

fn fmt_price(cents: i32, annual: bool) -> String {
    if cents == 0 {
        return "Free".to_string();
    }
    if annual {
        format!("${}/yr", cents / 100)
    } else {
        format!("${}/mo", cents / 100)
    }
}

#[component]
pub fn Billing() -> Element {
    let data = use_resource(|| async {
        let (status, plans) = futures_util::join!(api::billing::status(), api::billing::plans(),);
        (status.ok(), plans.unwrap_or_default())
    });

    let mut busy = use_signal(|| false);
    let mut annual = use_signal(|| false);
    // Two-click confirmation: clicking Upgrade arms this to the plan slug;
    // a second click on the same plan actually redirects to Stripe. Prevents
    // "whoops, didn't mean to upgrade" misclicks.
    let mut pending_upgrade = use_signal(|| None::<String>);

    rsx! {
        section { class: "flex flex-wrap justify-between items-start gap-4",
            div { class: "grid gap-1",
                span { class: "text-accent-100 text-xs uppercase tracking-widest", "Developer" }
                h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "Billing" }
            }
        }

        match &*data.read() {
            Some((Some(status), plans)) => {
                let remaining = status.credit_balance;
                let total = status.monthly_credits;
                let remaining_pct = if total > 0 {
                    ((remaining as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as u64
                } else {
                    0
                };
                let bar_color = if remaining_pct < 10 { "bg-accent-200" } else if remaining_pct < 30 { "bg-accent-100" } else { "bg-[#0F8B8D]" };
                let current_slug = status.plan_slug.clone();
                let is_annual = *annual.read();

                rsx! {
                    // Current plan + credit bar
                    section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-5",
                        div { class: "grid gap-1",
                            h2 { class: "text-[16px] font-mono font-normal text-fg", "{status.plan_name} plan" }
                            span { class: "text-base-500 text-[12px] font-mono",
                                "Status: {status.subscription_status}"
                            }
                        }

                        div { class: "grid gap-2",
                            div { class: "flex justify-between text-[12px] font-mono",
                                span { class: "text-base-400", "Credits remaining" }
                                {
                                    let rem_fmt = fmt_credits_exact(remaining);
                                    let total_fmt = fmt_credits_exact(total);
                                    rsx! { span { class: "text-fg", "{rem_fmt} / {total_fmt}" } }
                                }
                            }
                            div { class: "h-2 rounded-full bg-base-800 overflow-hidden",
                                div {
                                    class: "h-full rounded-full {bar_color} transition-all duration-500",
                                    style: "width: {remaining_pct}%",
                                }
                            }
                        }
                    }

                    // Stats
                    section { class: "grid grid-cols-[repeat(auto-fit,minmax(160px,1fr))] gap-4",
                        StatCard { label: "Credits remaining".to_string(), value: fmt_credits_exact(status.credit_balance) }
                        StatCard { label: "Monthly allowance".to_string(), value: fmt_credits_exact(status.monthly_credits) }
                        StatCard { label: "Plan".to_string(), value: status.plan_name.clone() }
                    }

                    BucketUsageSummary {}

                    // Plan cards
                    if !plans.is_empty() {
                        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
                            div { class: "flex items-center justify-between",
                                h2 { class: "text-[16px] font-mono font-normal text-fg", "Plans" }
                                // Monthly / Annual toggle
                                div { class: "flex items-center gap-0 rounded-full border border-base-800 p-0.5",
                                    button {
                                        class: if !is_annual { "px-3 py-1 rounded-full text-[11px] font-mono uppercase bg-base-800 text-fg transition" } else { "px-3 py-1 rounded-full text-[11px] font-mono uppercase text-base-500 hover:text-fg transition" },
                                        onclick: move |_| { annual.set(false); },
                                        "Monthly"
                                    }
                                    button {
                                        class: if is_annual { "px-3 py-1 rounded-full text-[11px] font-mono uppercase bg-base-800 text-fg transition" } else { "px-3 py-1 rounded-full text-[11px] font-mono uppercase text-base-500 hover:text-fg transition" },
                                        onclick: move |_| { annual.set(true); },
                                        "Annual"
                                        span { class: "ml-1 text-accent-100", "-10%" }
                                    }
                                }
                            }

                            div { class: "grid grid-cols-[repeat(auto-fit,minmax(200px,1fr))] gap-4",
                                for plan in plans.iter().filter(|p| p.slug != "enterprise") {
                                    {
                                        let is_current = plan.slug == current_slug;
                                        let is_free = plan.price_cents == 0;
                                        let has_stripe = plan.price_cents > 0;
                                        let monthly_price = fmt_price(plan.price_cents, false);
                                        let annual_price = fmt_price(plan.annual_price_cents, true);
                                        let would_cost_yearly = format!("${}/yr", plan.price_cents as i64 * 12 / 100);
                                        let credits_fmt = fmt_credits_short(plan.monthly_credits);
                                        let border = if is_current { "border-accent-100" } else { "border-base-800" };
                                        let slug = plan.slug.clone();
                                        let is_upgrade = has_stripe && (current_slug == "free" || plan.price_cents > plans.iter().find(|p| p.slug == current_slug).map(|p| p.price_cents).unwrap_or(0));
                                        let is_downgrade = !is_current && !is_upgrade;
                                        let show_annual = is_annual && has_stripe;

                                        rsx! {
                                            div { class: "rounded-lg border {border} bg-base-900 p-4 grid gap-3",
                                                div { class: "grid gap-2",
                                                    div { class: "flex items-center justify-between",
                                                        strong { class: "text-fg font-mono text-[14px]", "{plan.name}" }
                                                        if is_current {
                                                            span { class: "text-accent-100 text-[11px] font-mono uppercase", "Current" }
                                                        }
                                                    }
                                                    if !has_stripe {
                                                        span { class: "text-fg font-mono text-[20px] tracking-tight", "Free" }
                                                    } else if show_annual {
                                                        div { class: "flex items-baseline gap-2",
                                                            span { class: "text-accent-100 font-mono text-[20px] tracking-tight", "{annual_price}" }
                                                            span { class: "text-base-600 font-mono text-[13px] line-through", "{would_cost_yearly}" }
                                                        }
                                                    } else {
                                                        span { class: "text-fg font-mono text-[20px] tracking-tight", "{monthly_price}" }
                                                    }
                                                    span { class: "text-base-500 text-[12px] font-mono", "{credits_fmt} credits/month" }
                                                    span { class: "text-base-500 text-[11px] font-mono", "1 credit/read \u{b7} 10 credits/write" }
                                                }

                                                if is_current && has_stripe {
                                                    button {
                                                        class: "w-full px-3 py-1.5 rounded border border-base-700 text-base-400 hover:text-fg transition font-mono text-[11px] uppercase",
                                                        disabled: *busy.read(),
                                                        onclick: move |_| {
                                                            busy.set(true);
                                                            spawn(async move {
                                                                match api::billing::portal().await {
                                                                    Ok(url) => { if let Some(w) = web_sys::window() { let _ = w.location().set_href(&url); } }
                                                                    Err(_) => { busy.set(false); }
                                                                }
                                                            });
                                                        },
                                                        "Manage subscription"
                                                    }
                                                } else if is_upgrade {
                                                    {
                                                        let slug = slug.clone();
                                                        let is_pending = pending_upgrade.read().as_deref() == Some(slug.as_str());
                                                        let price_label = if is_annual { annual_price.clone() } else { monthly_price.clone() };
                                                        let plan_name = plan.name.clone();
                                                        rsx! {
                                                            if is_pending {
                                                                div { class: "grid gap-2",
                                                                    p { class: "text-base-400 font-mono text-[12px] leading-[140%] text-center",
                                                                        "About to subscribe to {plan_name} at {price_label}. You'll be redirected to Stripe to enter payment details."
                                                                    }
                                                                    div { class: "flex gap-2",
                                                                        button {
                                                                            class: "flex-1 px-3 py-1.5 rounded border border-base-700 text-base-400 hover:text-fg font-mono text-[11px] uppercase transition",
                                                                            onclick: move |_| { pending_upgrade.set(None); },
                                                                            "Cancel"
                                                                        }
                                                                        {
                                                                            let slug2 = slug.clone();
                                                                            rsx! {
                                                                                button {
                                                                                    class: "flex-1 px-3 py-1.5 rounded bg-accent-100 hover:bg-accent-200 text-[#1f1d1c] font-mono text-[11px] uppercase transition",
                                                                                    disabled: *busy.read(),
                                                                                    onclick: move |_| {
                                                                                        let slug = slug2.clone();
                                                                                        let ann = *annual.read();
                                                                                        busy.set(true);
                                                                                        spawn(async move {
                                                                                            match api::billing::checkout(&slug, ann).await {
                                                                                                Ok(url) => { if let Some(w) = web_sys::window() { let _ = w.location().set_href(&url); } }
                                                                                                Err(_) => { busy.set(false); }
                                                                                            }
                                                                                        });
                                                                                    },
                                                                                    "Continue to Stripe"
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            } else {
                                                                button {
                                                                    class: "w-full px-3 py-1.5 rounded bg-accent-100 hover:bg-accent-200 text-[#1f1d1c] font-mono text-[11px] uppercase transition",
                                                                    disabled: *busy.read(),
                                                                    onclick: move |_| { pending_upgrade.set(Some(slug.clone())); },
                                                                    "Upgrade"
                                                                }
                                                            }
                                                        }
                                                    }
                                                } else if is_downgrade && has_stripe {
                                                    button {
                                                        class: "w-full px-3 py-1.5 rounded border border-base-800 text-base-500 hover:text-fg transition font-mono text-[11px] uppercase",
                                                        disabled: *busy.read(),
                                                        onclick: move |_| {
                                                            busy.set(true);
                                                            spawn(async move {
                                                                match api::billing::portal().await {
                                                                    Ok(url) => { if let Some(w) = web_sys::window() { let _ = w.location().set_href(&url); } }
                                                                    Err(_) => { busy.set(false); }
                                                                }
                                                            });
                                                        },
                                                        "Downgrade"
                                                    }
                                                } else if is_current && is_free {
                                                    span { class: "text-center text-base-600 font-mono text-[11px] py-1.5", "Current plan" }
                                                }
                                            }
                                        }
                                    }
                                }
                                // Enterprise
                                Link {
                                    to: Route::Contact,
                                    class: "rounded-lg border border-dashed border-base-700 bg-base-900 p-4 grid gap-3 no-underline hover:border-accent-100 transition",
                                    div { class: "grid gap-2",
                                        strong { class: "text-fg font-mono text-[14px]", "Enterprise" }
                                        span { class: "text-base-400 text-[12px] font-mono", "Custom pricing" }
                                        span { class: "text-base-500 text-[12px] font-mono", "Unlimited credits, dedicated support, SLA" }
                                        span { class: "text-base-500 text-[11px] font-mono", "1 credit/read \u{b7} 10 credits/write" }
                                    }
                                    span { class: "w-full text-center px-3 py-1.5 rounded border border-dashed border-base-700 text-accent-100 font-mono text-[11px] uppercase", "Contact sales" }
                                }
                            }
                        }
                    }
                }
            },
            _ => rsx! { div { class: "text-base-500 text-center py-12", "Loading\u{2026}" } },
        }
    }
}

/// Top-5 buckets by event count. Lets a dev see where their credits are
/// going without opening every bucket individually. Uses the same
/// list_buckets endpoint the /buckets page already queries.
#[component]
fn BucketUsageSummary() -> Element {
    let data =
        use_resource(|| async { api::buckets::list_buckets("", 1, 15, "active").await.ok() });
    let data_read = data.read();
    let Some(Some(d)) = data_read.as_ref() else {
        return rsx! {};
    };
    if d.buckets.is_empty() {
        return rsx! {};
    }
    let mut ranked: Vec<_> = d.buckets.to_vec();
    ranked.sort_by(|a, b| b.event_count.unwrap_or(0).cmp(&a.event_count.unwrap_or(0)));
    let top: Vec<_> = ranked.into_iter().take(5).collect();
    let max = top.first().and_then(|b| b.event_count).unwrap_or(0).max(1);
    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
            div { class: "flex justify-between items-center",
                h2 { class: "text-[16px] font-mono font-normal text-fg", "Where your credits are going" }
                span { class: "text-base-500 font-mono text-[12px]", "Top buckets by event count" }
            }
            div { class: "grid gap-2",
                for b in top.iter() {
                    {
                        let events = b.event_count.unwrap_or(0);
                        let pct = (events as u64 * 100) / (max as u64).max(1);
                        let name = b.bucket.clone();
                        let count = events;
                        rsx! {
                            div { class: "grid gap-1",
                                div { class: "flex justify-between items-baseline font-mono text-[13px]",
                                    Link { to: Route::BucketDetail { bucket: name.clone() }, class: "text-accent-100 hover:text-fg no-underline", "{name}" }
                                    span { class: "text-base-500", "{count} events" }
                                }
                                div { class: "h-1.5 rounded-full bg-base-800 overflow-hidden",
                                    div { class: "h-full bg-[#0F8B8D]", style: "width: {pct}%" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
