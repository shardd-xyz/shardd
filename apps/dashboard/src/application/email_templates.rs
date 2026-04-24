use url::Url;

use crate::application::language::UserLanguage;

const BRAND_NAME: &str = "shardd";

// Factory-design tokens, inlined for email clients (no CSS vars).
// These mirror apps/dashboard-ui's globals.css dark theme exactly.
const BG: &str = "#020202";
const FG: &str = "#eee";
const BODY: &str = "#a49d9a"; // base-400
const MUTED: &str = "#8a8380"; // base-500
const SUBTLE: &str = "#5c5855"; // base-600
const DASHED: &str = "#4d4947"; // base-700
const BTN_BG: &str = "#1f1d1c"; // base-1000
const BTN_FG: &str = "#fafafa";
const ACCENT: &str = "#ef6f2e";

const FONT_STACK: &str =
    "'Geist Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace";

fn origin_label(app_origin: &str) -> String {
    Url::parse(app_origin)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_string()))
        .unwrap_or_else(|| app_origin.to_string())
}

/// Factory-design primary button: 4px radius, mono, uppercase, 12px, tight
/// tracking, flat dark fill.
pub fn primary_button(url: &str, label: &str) -> String {
    format!(
        r#"<a href="{url}" style="display:inline-block;padding:9px 14px;background-color:{BTN_BG};color:{BTN_FG};text-decoration:none;border:1px solid {DASHED};border-radius:4px;font-family:{FONT_STACK};font-size:12px;text-transform:uppercase;letter-spacing:-0.015rem;line-height:1;">{label}</a>"#
    )
}

/// Inline style for a raw-URL fallback paragraph that sits below the CTA.
/// Exposed so callers can avoid hardcoding color hexes.
pub fn fallback_paragraph(body: &str) -> String {
    format!(
        r#"<p style="margin:16px 0 0;font-family:{FONT_STACK};font-size:12px;line-height:140%;letter-spacing:-0.015rem;color:{MUTED};">{body}</p>"#
    )
}

/// Inline style for a plain body paragraph (transactional confirmation text).
pub fn body_paragraph(body: &str) -> String {
    format!(
        r#"<p style="margin:0 0 16px;font-family:{FONT_STACK};font-size:14px;line-height:140%;letter-spacing:-0.0175rem;color:{BODY};">{body}</p>"#
    )
}

/// Inline style to emphasize a URL inside copy: primary foreground, break-all.
pub fn url_span(url: &str) -> String {
    format!(r#"<span style="word-break:break-all;color:{FG};">{url}</span>"#)
}

pub fn wrap_email(
    lang: UserLanguage,
    app_origin: &str,
    headline: &str,
    lead: &str,
    body_html: &str,
    reason: &str,
    footer_note: Option<&str>,
) -> String {
    let origin = origin_label(app_origin);
    let copy = match lang {
        UserLanguage::En => (
            "Why you got this",
            "If you did not request this, you can safely ignore it.",
            "Sent by",
        ),
        UserLanguage::De => (
            "Grund für diese E-Mail",
            "Falls du das nicht warst, kannst du diese Nachricht ignorieren.",
            "Gesendet von",
        ),
    };
    let footer_note_html = footer_note
        .map(|note| {
            format!(
                r#"<p style="margin:8px 0 0;font-family:{FONT_STACK};font-size:12px;line-height:140%;letter-spacing:-0.015rem;color:{MUTED};">{note}</p>"#
            )
        })
        .unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <body style="background:{BG};margin:0;padding:32px 16px;font-family:{FONT_STACK};color:{FG};">
    <div style="max-width:560px;margin:0 auto;">
      <div style="font-family:{FONT_STACK};font-size:12px;text-transform:uppercase;letter-spacing:-0.015rem;color:{ACCENT};">{brand} · {origin}</div>
      <h1 style="margin:12px 0 16px;font-family:{FONT_STACK};font-weight:normal;font-size:28px;line-height:110%;letter-spacing:-0.02em;color:{FG};">{headline}</h1>
      <p style="margin:0 0 20px;font-family:{FONT_STACK};font-size:14px;line-height:140%;letter-spacing:-0.0175rem;color:{BODY};">{lead}</p>
      {body_html}
      <div style="margin-top:32px;padding-top:16px;border-top:1px dashed {DASHED};">
        <p style="margin:0 0 4px;font-family:{FONT_STACK};font-size:11px;text-transform:uppercase;letter-spacing:-0.015rem;color:{MUTED};">{reason_label}</p>
        <p style="margin:0 0 12px;font-family:{FONT_STACK};font-size:12px;line-height:140%;letter-spacing:-0.015rem;color:{BODY};">{reason}.</p>
        <p style="margin:0;font-family:{FONT_STACK};font-size:12px;line-height:140%;letter-spacing:-0.015rem;color:{MUTED};">{ignore_line}</p>
        {footer_note_html}
      </div>
      <p style="margin:16px 0 0;font-family:{FONT_STACK};font-size:11px;letter-spacing:-0.015rem;color:{SUBTLE};">{sent_by} {brand} · {origin}</p>
    </div>
  </body>
</html>
"#,
        brand = BRAND_NAME,
        origin = origin,
        headline = headline,
        lead = lead,
        body_html = body_html,
        reason = reason,
        reason_label = copy.0,
        ignore_line = copy.1,
        sent_by = copy.2,
        footer_note_html = footer_note_html,
    )
}
