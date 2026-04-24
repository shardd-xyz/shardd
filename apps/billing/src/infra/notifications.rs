use secrecy::ExposeSecret;
use tracing::{info, warn};

use crate::{adapters::http::app_state::AppState, application::billing::MeshClient};

const THRESHOLDS: &[(f64, &str, &str)] = &[
    (0.20, "20pct", "20%"),
    (0.10, "10pct", "10%"),
    (0.05, "5pct", "5%"),
    (0.00, "zero", "0"),
];

pub fn spawn_notification_loop(state: AppState) {
    tokio::spawn(async move {
        // Wait for services to settle before first check
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300)); // 5 min
        loop {
            interval.tick().await;
            if let Err(e) = check_all_users(&state).await {
                warn!(error = %e, "billing notification check failed");
            }
        }
    });
}

async fn check_all_users(state: &AppState) -> Result<(), Box<dyn std::error::Error>> {
    let subs = state.billing_repo.list_all_subscriptions().await?;
    let mesh = MeshClient::new(
        &state.config.gateway_url,
        state.config.gateway_machine_auth_secret.expose_secret(),
        &state.http_client,
    );

    for sub in &subs {
        let plan = match state.billing_repo.get_plan(sub.plan_id).await? {
            Some(p) => p,
            None => continue,
        };
        if plan.monthly_credits <= 0 {
            continue;
        }

        let balance = mesh.get_billing_balance(sub.user_id).await.unwrap_or(0);
        let ratio = balance as f64 / plan.monthly_credits as f64;

        // If balance was topped up (ratio > 0.5), clear old notifications so they can fire again
        if ratio > 0.5 {
            let _ = state.billing_repo.clear_notifications(sub.user_id).await;
            continue;
        }

        for &(threshold, key, label) in THRESHOLDS {
            if ratio <= threshold {
                if state
                    .billing_repo
                    .was_notification_sent(sub.user_id, key)
                    .await
                    .unwrap_or(true)
                {
                    continue;
                }

                let subject = if key == "zero" {
                    "Your shardd credits have run out".to_string()
                } else {
                    format!("Your shardd credits are below {label}")
                };

                let remaining_fmt = if balance <= 0 {
                    "0".to_string()
                } else {
                    balance.to_string()
                };
                let plan_fmt = plan.monthly_credits.to_string();

                // Factory-design-matched transactional email: flat (no
                // rounded wrapper box), mono stack, orange accent, dashed
                // divider. Colors inlined because Gmail/Outlook strip <style>
                // blocks and don't support CSS variables.
                const FONT_STACK: &str = "'Geist Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace";
                let cta_line = if key == "zero" {
                    "API requests are being rejected (402). Upgrade your plan or wait for the next billing cycle to restore access."
                } else {
                    "Consider upgrading your plan to avoid service interruption."
                };
                let html = format!(
                    r#"<!DOCTYPE html>
<html lang="en">
  <body style="background:#020202;margin:0;padding:32px 16px;font-family:{FONT_STACK};color:#eee;">
    <div style="max-width:560px;margin:0 auto;">
      <div style="font-family:{FONT_STACK};font-size:12px;text-transform:uppercase;letter-spacing:-0.015rem;color:#ef6f2e;">shardd · billing</div>
      <h1 style="margin:12px 0 16px;font-family:{FONT_STACK};font-weight:normal;font-size:28px;line-height:110%;letter-spacing:-0.02em;color:#eee;">Credit alert</h1>
      <p style="margin:0 0 16px;font-family:{FONT_STACK};font-size:14px;line-height:140%;letter-spacing:-0.0175rem;color:#a49d9a;">
        Your shardd account (<span style="color:#eee;">{email}</span>) has
        <span style="color:#eee;">{remaining_fmt}</span> of
        <span style="color:#eee;">{plan_fmt}</span> credits remaining on the
        <span style="color:#eee;">{plan_name}</span> plan.
      </p>
      <p style="margin:0 0 24px;font-family:{FONT_STACK};font-size:14px;line-height:140%;letter-spacing:-0.0175rem;color:#a49d9a;">{cta_line}</p>
      <a href="https://app.shardd.xyz/dashboard/billing" style="display:inline-block;padding:9px 14px;background-color:#1f1d1c;color:#fafafa;text-decoration:none;border:1px solid #4d4947;border-radius:4px;font-family:{FONT_STACK};font-size:12px;text-transform:uppercase;letter-spacing:-0.015rem;line-height:1;">Manage billing</a>
      <div style="margin-top:32px;padding-top:16px;border-top:1px dashed #4d4947;">
        <p style="margin:0;font-family:{FONT_STACK};font-size:11px;letter-spacing:-0.015rem;color:#5c5855;">shardd control plane</p>
      </div>
    </div>
  </body>
</html>"#,
                    email = sub.user_email,
                    plan_name = plan.name,
                );

                if let Err(e) = send_email(state, &sub.user_email, &subject, &html).await {
                    warn!(user = %sub.user_email, threshold = key, error = %e, "failed to send billing notification");
                } else {
                    info!(user = %sub.user_email, threshold = key, balance, "sent billing notification");
                    let _ = state
                        .billing_repo
                        .mark_notification_sent(sub.user_id, key)
                        .await;
                }
            }
        }
    }

    Ok(())
}

async fn send_email(
    state: &AppState,
    to: &str,
    subject: &str,
    html: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(serde::Serialize)]
    struct ResendReq<'a> {
        from: &'a str,
        to: [&'a str; 1],
        subject: &'a str,
        html: &'a str,
    }

    state
        .http_client
        .post("https://api.resend.com/emails")
        .bearer_auth(state.config.resend_api_key.expose_secret())
        .json(&ResendReq {
            from: &state.config.email_from,
            to: [to],
            subject,
            html,
        })
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
