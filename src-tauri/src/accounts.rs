//! Account management: Offline, LittleSkin (Yggdrasil) and Ely.by
//! (Yggdrasil authserver) providers. Tokens are persisted to `accounts.json`
//! in the launcher data dir; passwords are never stored — they are only sent
//! once over HTTPS to the chosen auth server.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Offline,
    LittleSkin,
    ElyBy,
}

impl Provider {
    pub fn label(&self) -> &'static str {
        match self {
            Provider::Offline => "Offline",
            Provider::LittleSkin => "LittleSkin",
            Provider::ElyBy => "Ely.by",
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Offline => "offline",
            Provider::LittleSkin => "littleskin",
            Provider::ElyBy => "elyby",
        }
    }
}

/// A sign-in that can produce a Minecraft session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub provider: Provider,
    pub username: String,
    /// UUID in dashed form (stripped to dashless when building launch args).
    pub uuid: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_token: Option<String>,
}

impl Account {
    pub fn user_type(&self) -> &'static str {
        match self.provider {
            Provider::Offline => "legacy",
            Provider::LittleSkin => "littleskin",
            Provider::ElyBy => "elyby",
        }
    }
    pub fn uuid_dashless(&self) -> String {
        self.uuid.replace('-', "")
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("{0}")]
    Msg(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Mojang-compatible offline UUID derived from a player name (MD5 of
/// "OfflinePlayer:<name>" with the version nibbles set). This is what
/// vanilla single-player uses, so the same name always maps to the same caped
/// profile everywhere.
pub fn offline_uuid(username: &str) -> String {
    use md5::compute;
    let digest = compute(format!("OfflinePlayer:{username}"));
    let mut b = digest.to_vec();
    // Set UUID v3-ish version bits the same way Mojang does.
    b[6] = (b[6] & 0x0f) | 0x30;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15]
    )
}

pub fn make_offline(username: &str) -> Account {
    let name = username.trim();
    let name = if name.is_empty() { "AlyrionPlayer" } else { name };
    Account {
        provider: Provider::Offline,
        username: name.to_string(),
        uuid: offline_uuid(name),
        access_token: "0".to_string(),
        refresh_token: None,
        client_token: None,
    }
}

/// Lightweight v4-ish UUID (good enough as client token / state).
pub fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let a = (nanos & 0xffff_ffff_ffff) as u64;
    let b = (nanos >> 48) as u64;
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (a >> 16) as u32,
        (a & 0xffff) as u16,
        ((b >> 24) & 0xfff) as u16,
        (b & 0xffff) as u16,
        (a % 0xffff_ffff_ffff)
    )
}

/// LittleSkin / Yggdrasil authenticate. The server base defaults to
/// `https://littleskin.cn/api/yggdrasil` and can be overridden in
/// settings.json (`littleskin_server`).
pub async fn littleskin_authenticate(
    client: &reqwest::Client,
    server: &str,
    username: &str,
    password: &str,
) -> Result<Account, AuthError> {
    let url = format!("{server}/authserver/authenticate");
    let client_token = uuid_v4();
    let body = serde_json::json!({
        "username": username,
        "password": password,
        "clientToken": client_token,
        "requestUser": true
    });
    let resp = client.post(&url).json(&body).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AuthError::Msg(format!(
            "LittleSkin login failed ({status}): {text}"
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let access = v
        .get("accessToken")
        .and_then(|x| x.as_str())
        .ok_or_else(|| AuthError::Msg("LittleSkin: missing accessToken".into()))?
        .to_string();
    let (name, uid) = match v.get("selectedProfile") {
        Some(sel) if sel.is_object() => (
            sel.get("name").and_then(|x| x.as_str()).unwrap_or(username).to_string(),
            sel.get("id").and_then(|x| x.as_str()).unwrap_or(&offline_uuid(username)).to_string(),
        ),
        _ => (username.to_string(), offline_uuid(username)),
    };
    Ok(Account {
        provider: Provider::LittleSkin,
        username: name,
        uuid: uid,
        access_token: access,
        refresh_token: None,
        client_token: Some(client_token),
    })
}

/// Refresh a LittleSkin access token (token stays valid, only refreshToken
/// changes). Keeps the same clientToken.
pub async fn littleskin_refresh(
    client: &reqwest::Client,
    server: &str,
    account: &Account,
) -> Result<Account, AuthError> {
    let url = format!("{server}/authserver/refresh");
    let body = serde_json::json!({
        "accessToken": account.access_token,
        "clientToken": account.client_token,
    });
    let resp = client.post(&url).json(&body).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AuthError::Msg(format!(
            "LittleSkin refresh failed ({status}): {text}"
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let access = v
        .get("accessToken")
        .and_then(|x| x.as_str())
        .unwrap_or(&account.access_token)
        .to_string();
    let uid = v
        .get("selectedProfile")
        .and_then(|s| s.get("id"))
        .and_then(|x| x.as_str())
        .unwrap_or(&account.uuid)
        .to_string();
    let name = v
        .get("selectedProfile")
        .and_then(|s| s.get("name"))
        .and_then(|x| x.as_str())
        .unwrap_or(&account.username)
        .to_string();
    Ok(Account {
        provider: Provider::LittleSkin,
        username: name,
        uuid: uid,
        access_token: access,
        refresh_token: account.refresh_token.clone(),
        client_token: account.client_token.clone(),
    })
}

/// Ely.by Yggdrasil authserver root (Mojang-compatible — same as XMCL et al).
pub const ELYBY_AUTHSERVER: &str = "https://authserver.ely.by";

/// Direct-credential login against Ely.by's Mojang-compatible authserver
/// (`POST /auth/authenticate`). No OAuth app, no client id/secret, no
/// browser — exactly how XMCL and other launchers do it.
///
/// The password is sent once over HTTPS and never stored; only the
/// accessToken + clientToken are kept. If the account has 2FA enabled the
/// server answers 401 `ForbiddenOperationException`; the caller should then
/// pass `password:totp` to retry.
pub async fn elyby_authenticate(
    client: &reqwest::Client,
    username: &str,
    password: &str,
) -> Result<Account, AuthError> {
    let client_token = uuid_v4();
    let body = serde_json::json!({
        "username": username,
        "password": password,
        "clientToken": client_token,
        "requestUser": true,
    });
    let resp = client
        .post(format!("{ELYBY_AUTHSERVER}/auth/authenticate"))
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        // Surface the server's human-readable errorMessage when present.
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("errorMessage")
                    .and_then(|x| x.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| text.clone());
        return Err(AuthError::Msg(format!(
            "Ely.by login failed ({status}): {msg}"
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let access = v
        .get("accessToken")
        .and_then(|x| x.as_str())
        .ok_or_else(|| AuthError::Msg("Ely.by: missing accessToken".into()))?
        .to_string();
    let (name, uid) = match v.get("selectedProfile") {
        Some(sel) if sel.is_object() => (
            sel.get("name")
                .and_then(|x| x.as_str())
                .unwrap_or(username)
                .to_string(),
            sel.get("id")
                .and_then(|x| x.as_str())
                .unwrap_or(&offline_uuid(username))
                .to_string(),
        ),
        _ => (username.to_string(), offline_uuid(username)),
    };
    Ok(Account {
        provider: Provider::ElyBy,
        username: name,
        uuid: uid,
        access_token: access,
        refresh_token: None,
        client_token: Some(client_token),
    })
}

/// Refresh an Ely.by access token via `POST /auth/refresh` (no password
/// needed, keeps the user signed in across sessions).
pub async fn elyby_refresh(
    client: &reqwest::Client,
    account: &Account,
) -> Result<Account, AuthError> {
    let access = account
        .access_token
        .clone();
    let client_token = account.client_token.clone().unwrap_or_else(uuid_v4);
    let body = serde_json::json!({
        "accessToken": access,
        "clientToken": client_token,
        "requestUser": true,
    });
    let resp = client
        .post(format!("{ELYBY_AUTHSERVER}/auth/refresh"))
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("errorMessage").and_then(|x| x.as_str()).map(String::from))
            .unwrap_or_else(|| text.clone());
        return Err(AuthError::Msg(format!(
            "Ely.by session expired ({status}): {msg} — please log in again"
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let new_access = v
        .get("accessToken")
        .and_then(|x| x.as_str())
        .unwrap_or(&account.access_token)
        .to_string();
    let uid = v
        .get("selectedProfile")
        .and_then(|s| s.get("id"))
        .and_then(|x| x.as_str())
        .unwrap_or(&account.uuid)
        .to_string();
    let name = v
        .get("selectedProfile")
        .and_then(|s| s.get("name"))
        .and_then(|x| x.as_str())
        .unwrap_or(&account.username)
        .to_string();
    Ok(Account {
        provider: Provider::ElyBy,
        username: name,
        uuid: uid,
        access_token: new_access,
        refresh_token: None,
        client_token: Some(client_token),
    })
}

/// Load settings.json from the launcher base dir. Returns a default if
/// missing or malformed.
pub fn load_settings(base_dir: &std::path::Path) -> Settings {
    let path = base_dir.join("settings.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// LittleSkin Yggdrasil server base.
    #[serde(default = "default_littleskin_server")]
    pub littleskin_server: String,
    /// Allocated RAM in megabytes.
    #[serde(default = "default_allocated_memory_mb")]
    pub allocated_memory_mb: u32,
    /// Custom extra JVM arguments.
    #[serde(default)]
    pub jvm_args: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            littleskin_server: default_littleskin_server(),
            allocated_memory_mb: default_allocated_memory_mb(),
            jvm_args: String::new(),
        }
    }
}

pub fn default_allocated_memory_mb() -> u32 {
    4096
}

pub fn default_littleskin_server() -> String {
    "https://littleskin.cn/api/yggdrasil".to_string()
}

/// Persist settings.json to disk.
pub fn save_settings(base_dir: &std::path::Path, settings: &Settings) -> Result<(), AuthError> {
    let path = base_dir.join("settings.json");
    let text = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// Load accounts from disk.
pub fn load_accounts(base_dir: &std::path::Path) -> Vec<Account> {
    let path = base_dir.join("accounts.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Account>>(&t).ok())
        .unwrap_or_default()
}

/// Persist accounts. Never stores passwords.
pub fn save_accounts(base_dir: &std::path::Path, accounts: &[Account]) -> Result<(), AuthError> {
    let path = base_dir.join("accounts.json");
    let text = serde_json::to_string_pretty(accounts)?;
    std::fs::write(path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_uuid_is_stable() {
        assert_eq!(
            offline_uuid("AlyrionPlayer"),
            offline_uuid("AlyrionPlayer")
        );
        assert_ne!(offline_uuid("AlyrionPlayer"), offline_uuid("Steve"));
        assert_eq!(offline_uuid("AlyrionPlayer").len(), 36);
    }

    #[test]
    fn dashless_works() {
        let a = make_offline("Test");
        assert_eq!(a.uuid_dashless().len(), 32);
        assert!(!a.uuid_dashless().contains('-'));
    }
}