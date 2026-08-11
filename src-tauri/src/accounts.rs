//! Account management: Offline, LittleSkin (Yggdrasil) and Ely.by (OAuth2)
//! providers. Tokens are persisted to `accounts.json` in the launcher data
//! dir; passwords are never stored (LittleSkin Yggdrasil flow uses the
//! access token afterwards; the password is only sent once over HTTPS).

use serde::{Deserialize, Serialize};
use std::time::Duration;
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

/// Percent-encode for query strings (like `redirect_uri=http%3A%2F%2F…`).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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

pub const ELYBY_REDIRECT: &str = "http://127.0.0.1:17423/callback";
pub const ELYBY_REDIRECT_PORT: u16 = 17423;

/// Ely.by OAuth2 code flow:
///  1. open the authorize page in the system browser with a localhost
///     redirect URI (registered as `ELYBY_REDIRECT`)
///  2. a loopback HTTP listener on the fixed port captures the `code`
///  3. exchange code → access_token at the token endpoint (secret required)
///  4. fetch the current user to get username/uuid
///
/// Ely.by does NOT support public clients / PKCE — the `client_secret` is
/// mandatory for both the code exchange and the refresh grant. The secret is
/// therefore read from the user's local `settings.json` (`elyby_client_secret`)
/// and is never compiled into the binary or committed. See README.
pub async fn elyby_login(
    client: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
) -> Result<Account, AuthError> {
    if client_id.trim().is_empty() || client_id == "your-elyby-client-id" {
        return Err(AuthError::Msg(
            "Ely.by client id is not configured. Put your Ely.by OAuth app \
             client id under `elyby_client_id` in settings.json (see README)."
                .into(),
        ));
    }
    if client_secret.trim().is_empty() || client_secret == "your-elyby-client-secret" {
        return Err(AuthError::Msg(
            "Ely.by client secret is not configured. Put your Ely.by OAuth app \
             client secret under `elyby_client_secret` in settings.json — it \
             never ships in the binary and you should rotate it if it ever leaks."
                .into(),
        ));
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", ELYBY_REDIRECT_PORT))
        .await
        .map_err(|e| AuthError::Msg(format!("cannot listen for Ely.by redirect: {e}")))?;
    let redirect = ELYBY_REDIRECT.to_string();
    let state = uuid_v4();
    // Percent-encode the redirect URI (Ely.by expects a query param).
    let redirect_enc = urlencode(&redirect);
    let auth_url = format!(
        "https://account.ely.by/oauth2/v1?client_id={client_id}\
         &redirect_uri={redirect_enc}&response_type=code\
         &scope=account_info%20offline_access&state={state}"
    );

    tauri_plugin_opener::open_url(&auth_url, None::<&str>)
        .map_err(|e| AuthError::Msg(format!("cannot open browser: {e}")))?;

    // Wait for the browser to bounce back to our loopback listener.
    let (mut socket, _) =
        tokio::time::timeout(Duration::from_secs(300), listener.accept())
            .await
            .map_err(|_| AuthError::Msg("timed out waiting for Ely.by login".into()))??;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = [0u8; 8192];
    let n = socket.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]).to_string();

    // `GET /callback?code=...&state=... HTTP/1.1`
    let query = req
        .lines()
        .next()
        .and_then(|line| {
            line.split_whitespace()
                .nth(1)
                .and_then(|p| p.split('?').nth(1))
        })
        .unwrap_or_default();
    let mut params = std::collections::HashMap::new();
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            params.insert(k.to_string(), v.to_string());
        }
    }
    // CSRF defense: the `state` echoed back must match the one we issued.
    if params.get("state").map(String::as_str) != Some(state.as_str()) {
        return Err(AuthError::Msg(
            "Ely.by login rejected: state mismatch (possible CSRF). Try again.".into(),
        ));
    }
    let code = params
        .get("code")
        .cloned()
        .ok_or_else(|| AuthError::Msg("Ely.by callback did not contain a code".into()))?;

    // Respond to the browser so the user can close the tab.
    let _ = socket
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\
              Content-Length: 47\r\nConnection: close\r\n\r\n\
              You can close this tab and return to the launcher.",
        )
        .await;

    // Exchange code for token.
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("redirect_uri", redirect.as_str()),
        ("code", code.as_str()),
    ];
    let resp = client
        .post("https://account.ely.by/api/oauth2/v1/token")
        .form(&params)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AuthError::Msg(format!(
            "Ely.by token exchange failed ({status}): {text}"
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| AuthError::Msg("Ely.by: missing access_token".into()))?
        .to_string();
    let refresh = v.get("refresh_token").and_then(|x| x.as_str()).map(String::from);

    // Fetch the current user.
    let resp = client
        .get("https://account.ely.by/api/account/v1/info")
        .bearer_auth(&access)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AuthError::Msg(format!(
            "Ely.by user lookup failed ({status}): {text}"
        )));
    }
    let user: serde_json::Value = serde_json::from_str(&text)?;
    let username = user
        .get("username")
        .or_else(|| user.get("name"))
        .and_then(|x| x.as_str())
        .unwrap_or("ElyByPlayer")
        .to_string();
    let uuid = user
        .get("uuid")
        .and_then(|x| x.as_str())
        .map(String::from)
        .unwrap_or_else(|| offline_uuid(&username));

    Ok(Account {
        provider: Provider::ElyBy,
        username,
        uuid,
        access_token: access,
        refresh_token: refresh,
        client_token: None,
    })
}

/// Renew an Ely.by token (keeps user signed in across sessions).
/// Requires `offline_access` scope (requested at login) and the client secret.
pub async fn elyby_refresh(
    client: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    account: &Account,
) -> Result<Account, AuthError> {
    let rt = account
        .refresh_token
        .clone()
        .ok_or_else(|| AuthError::Msg("no refresh token".into()))?;
    if client_secret.trim().is_empty() || client_secret == "your-elyby-client-secret" {
        return Err(AuthError::Msg(
            "Ely.by client secret is not configured (settings.json \
             `elyby_client_secret`) — cannot refresh the session.".into(),
        ));
    }
    let params = [
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("scope", "account_info offline_access"),
        ("refresh_token", rt.as_str()),
    ];
    let resp = client
        .post("https://account.ely.by/api/oauth2/v1/token")
        .form(&params)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AuthError::Msg(format!(
            "Ely.by refresh failed ({status}): {text}"
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .unwrap_or(&account.access_token)
        .to_string();
    let refresh = v.get("refresh_token").and_then(|x| x.as_str()).map(String::from);
    Ok(Account {
        provider: Provider::ElyBy,
        username: account.username.clone(),
        uuid: account.uuid.clone(),
        access_token: access,
        refresh_token: refresh.or(account.refresh_token.clone()),
        client_token: None,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    /// Ely.by OAuth app client id (owner registers at account.ely.by).
    #[serde(default)]
    pub elyby_client_id: String,
    /// Ely.by OAuth app client secret — lives ONLY in settings.json, never
    /// in the repo or binary. Rotate it if it leaks.
    #[serde(default)]
    pub elyby_client_secret: String,
    /// LittleSkin Yggdrasil server base.
    #[serde(default = "default_littleskin_server")]
    pub littleskin_server: String,
}

fn default_littleskin_server() -> String {
    "https://littleskin.cn/api/yggdrasil".to_string()
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