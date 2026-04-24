use async_trait::async_trait;
use reqwest::Client;
use serde::Serialize;

use crate::{
    app_error::{AppError, AppResult},
    use_cases::user::EmailSender,
};
use secrecy::ExposeSecret;

#[derive(Clone)]
pub struct ResendEmailSender {
    client: Client,
    api_key: secrecy::SecretString,
    from: String,
}

impl ResendEmailSender {
    pub fn new(api_key: secrecy::SecretString, from: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            from,
        }
    }
}

#[derive(Serialize)]
struct ResendReq<'a> {
    from: &'a str,
    to: [&'a str; 1],
    subject: &'a str,
    html: &'a str,
}

#[async_trait]
impl EmailSender for ResendEmailSender {
    async fn send(&self, to: &str, subject: &str, html: &str) -> AppResult<()> {
        let body = ResendReq {
            from: &self.from,
            to: [to],
            subject,
            html,
        };
        self.client
            .post("https://api.resend.com/emails")
            .bearer_auth(self.api_key.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?
            .error_for_status()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(())
    }
}
