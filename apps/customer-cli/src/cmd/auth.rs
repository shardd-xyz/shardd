//! `shardd auth login | logout | whoami` — device-flow auth.

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::credentials::{self, Credentials};
use crate::http::DashboardClient;
use crate::output::print_json;

#[derive(Subcommand)]
pub enum AuthCmd {
    /// Open a browser, authorize this CLI as your account, paste the
    /// returned code back here. Stores the resulting API key at
    /// ~/.config/shardd/credentials.toml (0600).
    Login,
    /// Forget the stored credentials. Also revokes the corresponding
    /// API key on the server unless --local is passed.
    Logout {
        /// Skip the server-side revoke; only delete the local file.
        #[arg(long)]
        local: bool,
    },
    /// Print the currently logged-in account (calls /api/developer/me).
    Whoami,
}

pub async fn run(cmd: AuthCmd, dashboard_url_override: Option<&str>) -> Result<()> {
    match cmd {
        AuthCmd::Login => login(dashboard_url_override).await,
        AuthCmd::Logout { local } => logout(local, dashboard_url_override).await,
        AuthCmd::Whoami => whoami(dashboard_url_override).await,
    }
}

#[derive(Serialize)]
struct StartRequest {
    client_name: String,
    hostname: String,
}

#[derive(Deserialize)]
struct StartResponse {
    session_id: String,
    verification_uri: String,
}

#[derive(Serialize)]
struct ExchangeRequest {
    session_id: String,
    verification_code: String,
}

#[derive(Deserialize)]
struct ExchangeResponse {
    api_key: String,
    key_id: String,
    user_id: String,
    email: String,
}

async fn login(dashboard_url_override: Option<&str>) -> Result<()> {
    let dashboard_url = credentials::dashboard_url(dashboard_url_override);
    let client = DashboardClient::new(dashboard_url.clone(), None)?;

    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let client_name = format!("shardd-cli/{}", env!("CARGO_PKG_VERSION"));

    let start: StartResponse = client
        .request_json::<StartResponse, StartRequest>(
            Method::POST,
            "/api/auth/cli/start",
            Some(&StartRequest {
                client_name: client_name.clone(),
                hostname: hostname.clone(),
            }),
        )
        .await
        .context("start cli auth session")?;

    eprintln!();
    eprintln!("    Open this URL in your browser:");
    eprintln!();
    eprintln!("      {}", start.verification_uri);
    eprintln!();
    if let Err(err) = webbrowser::open(&start.verification_uri) {
        eprintln!("    (couldn't auto-open the browser: {err})");
    }
    eprintln!("    Hit Authorize, then paste the code shown on the page below.");
    eprintln!();

    let code =
        rpassword::prompt_password("    code: ").context("read verification code from stdin")?;
    let code = code.trim();
    if code.is_empty() {
        return Err(anyhow!("no code entered — aborting"));
    }

    let exchanged: ExchangeResponse = client
        .request_json::<ExchangeResponse, ExchangeRequest>(
            Method::POST,
            "/api/auth/cli/exchange",
            Some(&ExchangeRequest {
                session_id: start.session_id,
                verification_code: code.to_string(),
            }),
        )
        .await
        .context("exchange verification code")?;

    let creds = Credentials {
        api_key: exchanged.api_key,
        key_id: exchanged.key_id,
        user_id: exchanged.user_id,
        email: exchanged.email.clone(),
        dashboard_url,
    };
    credentials::save(&creds)?;

    eprintln!();
    eprintln!("    Logged in as {}", exchanged.email);
    eprintln!(
        "    Credentials saved to {}",
        credentials::credentials_path()?.display()
    );
    Ok(())
}

async fn logout(local_only: bool, dashboard_url_override: Option<&str>) -> Result<()> {
    let creds = match credentials::load_optional() {
        Some(c) => c,
        None => {
            eprintln!("    Already logged out.");
            return Ok(());
        }
    };

    if !local_only {
        let dashboard_url = dashboard_url_override
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                if creds.dashboard_url.is_empty() {
                    credentials::DEFAULT_DASHBOARD_URL.to_string()
                } else {
                    creds.dashboard_url.clone()
                }
            });
        let client = DashboardClient::new(dashboard_url, Some(creds.api_key.clone()))?;
        let path = format!("/api/developer/keys/{}/revoke", creds.key_id);
        if let Err(err) = client
            .request_no_content(Method::POST, &path, None::<&Value>)
            .await
        {
            eprintln!(
                "    Warning: server-side revoke failed ({err}). The local credentials will still be deleted."
            );
        }
    }

    credentials::delete()?;
    eprintln!("    Logged out.");
    Ok(())
}

async fn whoami(dashboard_url_override: Option<&str>) -> Result<()> {
    let creds = credentials::load()?;
    let dashboard_url = dashboard_url_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if creds.dashboard_url.is_empty() {
                credentials::DEFAULT_DASHBOARD_URL.to_string()
            } else {
                creds.dashboard_url.clone()
            }
        });
    let client = DashboardClient::new(dashboard_url, Some(creds.api_key.clone()))?;
    let me: Value = client
        .request_value(Method::GET, "/api/developer/me", None)
        .await?;
    print_json(&json!({
        "email": creds.email,
        "user_id": creds.user_id,
        "key_id": creds.key_id,
        "developer_account": me,
    }))
}
