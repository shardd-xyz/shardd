use crate::api;
use crate::components::button::{Btn, BtnSize, BtnVariant};
use crate::components::meta_row::{MetaRow, MetaRowCode};
use crate::router::Route;
use crate::state::{use_notice, use_session};
use crate::types::{Notice, NoticeTone};
use dioxus::prelude::*;

#[component]
pub fn Profile() -> Element {
    let session = use_session();
    let s = session.read();
    let s = s.as_ref().unwrap();

    rsx! {
        div { class: "grid gap-6 w-full",
        section { class: "flex flex-wrap justify-between items-start gap-4",
            div { class: "grid gap-1",
                span { class: "text-accent-100 text-xs uppercase tracking-widest", "Account" }
                h1 { class: "text-[32px] font-normal leading-[100%] tracking-[-0.04em] font-mono tracking-tight", "Profile" }
            }
            div { class: "flex gap-2.5",
                Link { to: Route::Dashboard, class: "px-3.5 py-2 rounded-full border border-base-700 text-base-400 hover:text-fg transition no-underline text-sm", "Developer home" }
                if s.is_admin {
                    Link { to: Route::AdminOverview, class: "px-3.5 py-2 rounded-full border border-base-700 text-base-400 hover:text-fg transition no-underline text-sm", "Admin overview" }
                }
            }
        }

        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-4",
            h2 { class: "text-[16px] font-normal", "Profile details" }
            div {
                MetaRow { label: "Email".to_string(), value: s.email.clone() }
                MetaRow { label: "Role".to_string(), value: if s.is_admin { "Admin".to_string() } else { "Developer".to_string() } }
                MetaRowCode { label: "User ID".to_string(), value: s.id.clone() }
            }
        }

        ProfileFieldsSection {}
        DataExportSection {}
        DeleteAccountSection { email: s.email.clone() }
        }
    }
}

#[component]
fn ProfileFieldsSection() -> Element {
    let mut notice = use_notice();
    let me = use_resource(|| async { api::developer::me().await.ok() });
    let mut display_name = use_signal(String::new);
    let mut hydrated = use_signal(|| false);
    let mut saving = use_signal(|| false);

    // Hydrate the input once the /me response lands. Subsequent edits stay
    // local until Save.
    if !*hydrated.read()
        && let Some(Some(p)) = me.read().as_ref()
    {
        display_name.set(p.display_name.clone().unwrap_or_default());
        hydrated.set(true);
    }

    let on_save = move |evt: FormEvent| {
        evt.prevent_default();
        let name = display_name.read().clone();
        saving.set(true);
        spawn(async move {
            match api::developer::update_profile(Some(&name), None).await {
                Ok(_) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Success,
                        "Profile saved",
                        "Your display name has been updated.",
                    )));
                }
                Err(e) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Danger,
                        "Couldn't save profile",
                        e.friendly().1,
                    )));
                }
            }
            saving.set(false);
        });
    };

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-3",
            h2 { class: "text-[16px] font-normal", "Edit profile" }
            form { class: "grid gap-3", onsubmit: on_save,
                div { class: "grid gap-1",
                    label { class: "font-mono text-[12px] uppercase tracking-[-0.015rem] text-base-500", "Display name" }
                    input {
                        r#type: "text",
                        placeholder: "How do you want to be addressed?",
                        value: "{display_name}",
                        oninput: move |e| display_name.set(e.value()),
                    }
                    p { class: "font-mono text-[11px] text-base-600 leading-[140%]",
                        "Shown in emails and internal UIs. Leave empty to fall back to your email address."
                    }
                }
                div { class: "flex justify-end",
                    Btn {
                        r#type: "submit".to_string(),
                        variant: BtnVariant::Primary,
                        size: BtnSize::Sm,
                        disabled: *saving.read(),
                        if *saving.read() { "Saving\u{2026}" } else { "Save" }
                    }
                }
            }
        }
    }
}

#[component]
fn DataExportSection() -> Element {
    let mut notice = use_notice();
    let mut downloading = use_signal(|| false);
    let on_click = move |_| {
        downloading.set(true);
        spawn(async move {
            match api::developer::export_user_data_raw().await {
                Ok(json) => {
                    // Trigger a client-side download via object URL rather
                    // than round-tripping through a backend content-disposition
                    // header. JSON is small enough to live in-memory.
                    if let Some(w) = web_sys::window() {
                        let ts = js_sys::Date::new_0()
                            .to_iso_string()
                            .as_string()
                            .unwrap_or_default();
                        let filename = format!("shardd-export-{}.json", ts);
                        let _ = download_text_as_file(&w, &json, &filename);
                    }
                    notice.set(Some(Notice::new(
                        NoticeTone::Success,
                        "Export downloaded",
                        "Your account data is saved as JSON in your Downloads folder.",
                    )));
                }
                Err(e) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Danger,
                        "Export failed",
                        e.friendly().1,
                    )));
                }
            }
            downloading.set(false);
        });
    };

    rsx! {
        section { class: "rounded-lg border border-base-800 bg-base-900 p-6 grid gap-3",
            h2 { class: "text-[16px] font-normal", "Data export" }
            p { class: "font-mono text-[12px] text-base-500 leading-[140%]",
                "Download a JSON snapshot of your account: profile, API keys, scopes, and buckets. Event history lives on the mesh and is not included here."
            }
            div {
                Btn {
                    variant: BtnVariant::Primary,
                    size: BtnSize::Sm,
                    disabled: *downloading.read(),
                    onclick: on_click,
                    if *downloading.read() { "Preparing\u{2026}" } else { "Download data" }
                }
            }
        }
    }
}

fn download_text_as_file(
    window: &web_sys::Window,
    text: &str,
    filename: &str,
) -> Result<(), wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast;
    let blob_parts = js_sys::Array::new();
    blob_parts.push(&wasm_bindgen::JsValue::from_str(text));
    let blob = web_sys::Blob::new_with_str_sequence(&blob_parts)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let document = window.document().ok_or(wasm_bindgen::JsValue::NULL)?;
    let anchor = document
        .create_element("a")?
        .dyn_into::<web_sys::HtmlAnchorElement>()?;
    anchor.set_href(&url);
    anchor.set_download(filename);
    anchor.click();
    web_sys::Url::revoke_object_url(&url)?;
    Ok(())
}

#[component]
fn DeleteAccountSection(email: String) -> Element {
    let mut expanded = use_signal(|| false);
    let mut confirm = use_signal(String::new);
    let mut submitting = use_signal(|| false);
    let mut notice = use_notice();

    let email_cmp = email.clone();
    let matches = *confirm.read() == email_cmp;

    let on_delete = move |_| {
        submitting.set(true);
        spawn(async move {
            match api::auth::delete_account().await {
                Ok(_) => {
                    if let Some(window) = web_sys::window() {
                        let _ = window.location().set_href("/login");
                    }
                }
                Err(e) => {
                    notice.set(Some(Notice::new(
                        NoticeTone::Danger,
                        "Account delete failed",
                        e.friendly().1,
                    )));
                    submitting.set(false);
                }
            }
        });
    };

    rsx! {
        section { class: "rounded-lg border border-[#f87171]/30 bg-base-900 p-6 grid gap-3",
            h2 { class: "text-[16px] font-normal text-[#f87171]", "Danger zone" }
            if !*expanded.read() {
                p { class: "font-mono text-[12px] text-base-500 leading-[140%]",
                    "Deleting your account removes its data and cannot be undone. You must delete all of your buckets first."
                }
                div {
                    Btn {
                        variant: BtnVariant::Danger,
                        size: BtnSize::Sm,
                        onclick: move |_| expanded.set(true),
                        "Delete account"
                    }
                }
            } else {
                p { class: "font-mono text-[12px] text-base-400 leading-[140%]",
                    "Type your email to confirm:"
                }
                input {
                    r#type: "text",
                    placeholder: "{email}",
                    value: "{confirm}",
                    oninput: move |e| confirm.set(e.value()),
                }
                div { class: "flex items-center gap-2",
                    Btn {
                        variant: BtnVariant::Ghost,
                        size: BtnSize::Sm,
                        disabled: *submitting.read(),
                        onclick: move |_| {
                            expanded.set(false);
                            confirm.set(String::new());
                        },
                        "Cancel"
                    }
                    Btn {
                        variant: BtnVariant::Danger,
                        size: BtnSize::Sm,
                        disabled: !matches || *submitting.read(),
                        onclick: on_delete,
                        if *submitting.read() { "Deleting…" } else { "Delete forever" }
                    }
                }
            }
        }
    }
}
