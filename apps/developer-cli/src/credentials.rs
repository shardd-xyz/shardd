//! Local credentials storage at `~/.config/shardd/credentials.toml`.
//!
//! TOML so the file is human-readable for ops/debug. Permissions set
//! to 0600 on Unix to keep the API key out of `cat ~/.config/...`'s
//! reach by default. `dirs::config_dir()` gives the platform-correct
//! base — `$XDG_CONFIG_HOME` on Linux, `~/Library/Application Support`
//! on macOS, `%APPDATA%` on Windows.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

pub const DEFAULT_DASHBOARD_URL: &str = "https://app.shardd.xyz";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub api_key: String,
    pub key_id: String,
    pub user_id: String,
    pub email: String,
    /// Dashboard origin used for control-plane calls. Stored so the
    /// CLI keeps using the same URL across invocations after a one-off
    /// `SHARDD_DASHBOARD_URL=...` login (handy for local dev against
    /// `http://localhost:8080`).
    pub dashboard_url: String,
}

pub fn config_dir() -> Result<PathBuf> {
    let base =
        dirs::config_dir().ok_or_else(|| anyhow!("could not determine user config directory"))?;
    Ok(base.join("shardd"))
}

pub fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials.toml"))
}

pub fn load() -> Result<Credentials> {
    let path = credentials_path()?;
    let raw = fs::read_to_string(&path).with_context(|| {
        format!(
            "not logged in — run `shardd auth login` first ({} missing)",
            path.display()
        )
    })?;
    let creds: Credentials = toml::from_str(&raw).with_context(|| {
        format!(
            "could not parse {} — re-run `shardd auth login`",
            path.display()
        )
    })?;
    Ok(creds)
}

pub fn load_optional() -> Option<Credentials> {
    load().ok()
}

pub fn save(creds: &Credentials) -> Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = credentials_path()?;
    let contents = toml::to_string_pretty(creds).context("serialise credentials")?;
    fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
    set_secure_perms(&path)?;
    Ok(())
}

pub fn delete() -> Result<()> {
    let path = credentials_path()?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_secure_perms(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_secure_perms(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

pub fn dashboard_url(cli_override: Option<&str>) -> String {
    if let Some(u) = cli_override {
        return u.trim_end_matches('/').to_string();
    }
    if let Ok(env) = std::env::var("SHARDD_DASHBOARD_URL") {
        return env.trim_end_matches('/').to_string();
    }
    if let Some(c) = load_optional()
        && !c.dashboard_url.is_empty()
    {
        return c.dashboard_url.trim_end_matches('/').to_string();
    }
    DEFAULT_DASHBOARD_URL.to_string()
}
