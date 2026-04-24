use dioxus::prelude::*;

/// One-click copy to clipboard with a brief "Copied" confirmation.
///
/// Lifts the inline logic from `FlashKeyBanner` so every copy-worthy surface
/// (raw API keys, bucket names, user IDs, curl snippets) can share the same
/// interaction and visual language.
#[component]
pub fn CopyButton(
    value: String,
    label: Option<String>,
    on_copy: Option<EventHandler<()>>,
) -> Element {
    let mut copied = use_signal(|| false);
    let display = label.clone().unwrap_or_else(|| "Copy".to_string());
    let current = if *copied.read() {
        "Copied".to_string()
    } else {
        display
    };

    rsx! {
        button {
            r#type: "button",
            class: "text-base-500 hover:text-fg font-mono text-[12px] uppercase tracking-[-0.015rem] bg-transparent border-0 w-fit transition-colors duration-150 cursor-pointer",
            onclick: move |_| {
                let raw = value.clone();
                let on_copy = on_copy;
                spawn(async move {
                    let Some(window) = web_sys::window() else { return };
                    let clipboard = window.navigator().clipboard();
                    let promise = clipboard.write_text(&raw);
                    if wasm_bindgen_futures::JsFuture::from(promise).await.is_ok() {
                        copied.set(true);
                        if let Some(h) = on_copy.as_ref() {
                            h.call(());
                        }
                        gloo_timers::future::TimeoutFuture::new(1500).await;
                        copied.set(false);
                    }
                });
            },
            "{current}"
        }
    }
}
