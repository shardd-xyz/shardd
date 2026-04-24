use crate::types::{FlashKey, Notice, Session};
use dioxus::prelude::*;
use std::sync::OnceLock;

/// Initial `window.location.search` captured at WASM startup before the
/// Dioxus router has a chance to normalize the URL and drop query strings
/// on routes that don't declare them (e.g. `/magic?token=...`).
pub static INITIAL_SEARCH: OnceLock<String> = OnceLock::new();

pub fn use_session() -> Signal<Option<Session>> {
    use_context::<Signal<Option<Session>>>()
}

pub fn use_notice() -> Signal<Option<Notice>> {
    use_context::<Signal<Option<Notice>>>()
}

pub fn use_flash_key() -> Signal<Option<FlashKey>> {
    use_context::<Signal<Option<FlashKey>>>()
}

const FLASH_KEY_STORAGE: &str = "shardd.flash_key";

/// Read a previously-persisted FlashKey from sessionStorage.
///
/// The raw-key banner used to die on any page reload. We now persist it for
/// the lifetime of the browser tab (sessionStorage, not localStorage — the
/// secret must not survive the session) so a dev who accidentally navigates
/// away can still come back and copy it once.
pub fn load_flash_key_from_session() -> Option<FlashKey> {
    let window = web_sys::window()?;
    let storage = window.session_storage().ok()??;
    let raw = storage.get_item(FLASH_KEY_STORAGE).ok()??;
    serde_json::from_str(&raw).ok()
}

pub fn save_flash_key_to_session(flash: Option<&FlashKey>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(storage)) = window.session_storage() else {
        return;
    };
    match flash {
        Some(fk) => {
            if let Ok(serialized) = serde_json::to_string(fk) {
                let _ = storage.set_item(FLASH_KEY_STORAGE, &serialized);
            }
        }
        None => {
            let _ = storage.remove_item(FLASH_KEY_STORAGE);
        }
    }
}
