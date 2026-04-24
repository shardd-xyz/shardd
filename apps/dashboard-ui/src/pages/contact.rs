use crate::api;
use crate::components::button::{Btn, BtnSize, BtnVariant};
use crate::router::Route;
use crate::state::use_notice;
use crate::types::{Notice, NoticeTone};
use dioxus::prelude::*;

/// /dashboard/contact — replaces the old `mailto:emil@tqdm.org` Enterprise
/// link with a form that ends up in the ops inbox. Auth-gated so the user's
/// account gets attached automatically (no need to re-type identity).
#[component]
pub fn Contact() -> Element {
    let nav = use_navigator();
    let mut notice = use_notice();
    let topic = use_signal(default_topic);
    let mut company = use_signal(String::new);
    let mut team_size = use_signal(String::new);
    let mut volume = use_signal(String::new);
    let mut message = use_signal(String::new);
    let mut submitting = use_signal(|| false);

    let on_submit = move |evt: FormEvent| {
        evt.prevent_default();
        if message.read().trim().is_empty() {
            notice.set(Some(Notice::new(
                NoticeTone::Danger,
                "Message required",
                "Tell us what you're building or what you need.",
            )));
            return;
        }
        let req = api::auth::ContactRequest {
            topic: topic.read().clone(),
            company: Some(company.read().clone()).filter(|s| !s.is_empty()),
            team_size: Some(team_size.read().clone()).filter(|s| !s.is_empty()),
            volume: Some(volume.read().clone()).filter(|s| !s.is_empty()),
            message: message.read().clone(),
        };
        submitting.set(true);
        spawn(async move {
            match api::auth::send_contact(&req).await {
                Ok(()) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Success,
                        "Message sent",
                        "We'll get back to you at the email on your account.",
                    )));
                    nav.push(Route::Billing);
                }
                Err(e) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Danger,
                        "Could not send",
                        e.friendly().1,
                    )));
                }
            }
            submitting.set(false);
        });
    };

    rsx! {
        div { class: "grid gap-6 w-full",
            section { class: "flex flex-wrap justify-between items-start gap-4",
                div { class: "grid gap-1",
                    span { class: "text-accent-100 text-xs uppercase tracking-widest", "Support" }
                    h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "Contact" }
                }
                Link { to: Route::Billing, class: "px-3.5 py-2 rounded-full border border-base-700 text-base-400 hover:text-fg transition no-underline text-sm", "Back to billing" }
            }

            form {
                class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
                onsubmit: on_submit,
                p { class: "font-mono text-[13px] text-base-500 leading-[140%]",
                    "We usually reply within a business day. Your account email is on file; no need to repeat it here."
                }
                div { class: "grid gap-1",
                    label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Topic" }
                    TopicSelect { value: topic }
                }
                div { class: "grid gap-1",
                    label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Company (optional)" }
                    input {
                        r#type: "text",
                        placeholder: "Acme Inc.",
                        value: "{company}",
                        oninput: move |e| company.set(e.value()),
                    }
                }
                div { class: "grid grid-cols-2 gap-3 max-[640px]:grid-cols-1",
                    div { class: "grid gap-1",
                        label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Team size (optional)" }
                        input {
                            r#type: "text",
                            placeholder: "1-10",
                            value: "{team_size}",
                            oninput: move |e| team_size.set(e.value()),
                        }
                    }
                    div { class: "grid gap-1",
                        label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Est. events/month (optional)" }
                        input {
                            r#type: "text",
                            placeholder: "1M",
                            value: "{volume}",
                            oninput: move |e| volume.set(e.value()),
                        }
                    }
                }
                div { class: "grid gap-1",
                    label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Message" }
                    textarea {
                        rows: "5",
                        placeholder: "What are you building? What do you need?",
                        value: "{message}",
                        oninput: move |e| message.set(e.value()),
                    }
                }
                div { class: "flex justify-end",
                    Btn {
                        r#type: "submit".to_string(),
                        variant: BtnVariant::Primary,
                        size: BtnSize::Default,
                        disabled: *submitting.read(),
                        if *submitting.read() { "Sending\u{2026}" } else { "Send message" }
                    }
                }
            }
        }
    }
}

fn default_topic() -> String {
    if let Some(w) = web_sys::window()
        && let Ok(Some(search)) = w.location().search().map(Some)
    {
        if search.contains("plan=enterprise") {
            return "Enterprise plan".to_string();
        }
        if search.contains("plan=pro") {
            return "Pro plan question".to_string();
        }
    }
    "General".to_string()
}

#[component]
fn TopicSelect(value: Signal<String>) -> Element {
    rsx! {
        select {
            value: "{value}",
            onchange: move |e| value.set(e.value()),
            option { value: "General", "General" }
            option { value: "Enterprise plan", "Enterprise plan" }
            option { value: "Pro plan question", "Pro plan question" }
            option { value: "Technical question", "Technical question" }
            option { value: "Bug report", "Bug report" }
        }
    }
}
