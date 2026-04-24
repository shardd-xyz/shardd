pub fn format_relative_time(value: Option<u64>) -> String {
    let Some(ms) = value else { return "—".into() };
    if ms == 0 {
        return "—".into();
    }
    let now = js_sys::Date::now() as u64;
    let diff_sec = if now > ms { (now - ms) / 1000 } else { 0 };
    match diff_sec {
        0..=44 => "just now".into(),
        45..=89 => "1 min ago".into(),
        90..=2699 => format!("{} min ago", diff_sec / 60),
        2700..=5399 => "1 hour ago".into(),
        5400..=79199 => {
            let h = diff_sec / 3600;
            format!("{h} hour{} ago", if h == 1 { "" } else { "s" })
        }
        79200..=129599 => "1 day ago".into(),
        _ => {
            let days = diff_sec / 86400;
            if days < 14 {
                format!("{days} day{} ago", if days == 1 { "" } else { "s" })
            } else {
                format_date(Some(ms))
            }
        }
    }
}

pub fn format_relative_time_str(value: Option<&str>) -> String {
    let ms = value.and_then(|s| {
        let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(s));
        let t = d.get_time();
        if t.is_nan() { None } else { Some(t as u64) }
    });
    format_relative_time(ms)
}

pub fn format_date(value: Option<u64>) -> String {
    let Some(ms) = value else { return "—".into() };
    if ms == 0 {
        return "—".into();
    }
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms as f64));
    d.to_locale_string("en-US", &wasm_bindgen::JsValue::UNDEFINED)
        .as_string()
        .unwrap_or_else(|| "—".into())
}

pub fn format_date_str(value: Option<&str>) -> String {
    let ms = value.and_then(|s| {
        let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(s));
        let t = d.get_time();
        if t.is_nan() { None } else { Some(t as u64) }
    });
    format_date(ms)
}

pub fn format_amount(value: i64) -> String {
    value.to_string()
}

pub fn format_signed_amount(value: i64) -> String {
    if value > 0 {
        format!("+{value}")
    } else {
        value.to_string()
    }
}

pub fn amount_class(value: i64) -> &'static str {
    if value > 0 {
        "text-accent-100"
    } else if value < 0 {
        "text-accent-200"
    } else {
        "text-fg"
    }
}
