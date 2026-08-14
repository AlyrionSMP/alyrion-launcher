//! Modrinth API client for the Alyrion modpack.
//!
//! The launcher is locked to exactly one project: the Alyrion modpack
//! (`hqB4qj6d`). We never re-resolve any other project — the pack's own
//! .mrpack index drives every file that gets downloaded.

use serde::Deserialize;
use thiserror::Error;

pub const PACK_PROJECT_ID: &str = "hqB4qj6d";

const API_ROOT: &str = "https://api.modrinth.com/v2";

#[derive(Debug, Error)]
pub enum ModrinthError {
    #[error("modrinth api error: {0}")]
    Api(String),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("invalid response: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModrinthFile {
    pub hashes: FileHashes,
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileHashes {
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub sha512: Option<String>,
}

/// A released version of the modpack.
#[derive(Debug, Clone, Deserialize)]
pub struct PackVersion {
    pub id: String,
    #[serde(rename = "project_id")]
    pub project_id: String,
    #[serde(rename = "version_number")]
    pub version_number: String,
    #[serde(rename = "version_type")]
    pub version_type: String,
    #[serde(rename = "date_published")]
    pub date_published: String,
    /// Release notes in Markdown, as published on Modrinth.
    #[serde(default)]
    pub changelog: Option<String>,
    pub files: Vec<ModrinthFile>,
}

impl PackVersion {
    /// The primary .mrpack file for this version.
    pub fn primary_mrpack(&self) -> Option<&ModrinthFile> {
        self.files.iter().find(|f| f.primary)
    }
}

/// Fetches the latest released version of the Alyrion modpack.
pub async fn fetch_latest_version(client: &reqwest::Client) -> Result<PackVersion, ModrinthError> {
    let url = format!("{API_ROOT}/project/{PACK_PROJECT_ID}/version");
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(ModrinthError::Api(format!(
            "GET {url} -> {}",
            resp.status()
        )));
    }
    let versions: Vec<PackVersion> = resp.json().await?;
    let mut best = None;
    for v in &versions {
        if v.version_type == "release" {
            best = Some(v.clone());
            break;
        } else if v.version_type == "beta" && best.is_none() {
            best = Some(v.clone());
        }
    }
    best.or_else(|| versions.into_iter().next())
        .ok_or_else(|| ModrinthError::Invalid("no usable version found".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_version() {
        let json = r#"{
          "id":"frTF9alG",
          "project_id":"hqB4qj6d",
          "version_number":"7.4.0",
          "version_type":"release",
          "date_published":"2026-06-07T15:02:08.102761Z",
          "changelog":"line one\nline two",
          "files":[{"hashes":{"sha1":"abc","sha512":"def"},"url":"https://cdn.modrinth.com/x","filename":"Alyrion-7.4.0.mrpack","primary":true,"size":49042743}]
        }"#;
        let v: PackVersion = serde_json::from_str(json).unwrap();
        assert_eq!(v.version_number, "7.4.0");
        let f = v.primary_mrpack().unwrap();
        assert_eq!(f.filename, "Alyrion-7.4.0.mrpack");
        assert_eq!(f.hashes.sha1.as_deref(), Some("abc"));
    }
}