//! Parser for the `modrinth.index.json` payload that lives inside every
//! .mrpack archive. This drives which files to download, their hashes and
//! where the `overrides/` tree gets extracted.

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct IndexJson {
    #[serde(rename = "formatVersion")]
    pub format_version: u64,
    pub game: Option<String>,
    #[serde(rename = "versionId")]
    pub version_id: Option<String>,
    pub name: Option<String>,
    pub summary: Option<String>,
    pub dependencies: Dependencies,
    pub files: Vec<PackFile>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Dependencies {
    pub minecraft: Option<String>,
    pub neoforge: Option<String>,
    #[serde(default)]
    pub forge: Option<String>,
    #[serde(default)]
    pub quilt: Option<String>,
    #[serde(default)]
    pub fabric: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackFile {
    /// Path inside the instance dir, e.g. `mods/foo.jar`.
    pub path: String,
    pub hashes: FileHashes,
    /// `downloads` is either an array of URL strings or (rarely) an object.
    pub downloads: Downloads,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub env: Option<FileEnv>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileHashes {
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub sha512: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Downloads {
    Urls(Vec<String>),
    Object(HashMap<String, String>),
}

impl Downloads {
    pub fn first_url(&self) -> Option<&str> {
        match self {
            Downloads::Urls(v) => v.first().map(String::as_str),
            Downloads::Object(_) => None,
        }
    }
    pub fn is_empty(&self) -> bool {
        match self {
            Downloads::Urls(v) => v.is_empty(),
            Downloads::Object(m) => m.is_empty(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileEnv {
    pub client: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
}

impl PackFile {
    /// Should this file be installed on the client side?
    /// (env omitted/empty => always install; explicit client field wins.)
    pub fn needed_for_client(&self) -> bool {
        match &self.env {
            None => true,
            Some(e) => match &e.client {
                None => true,
                Some(v) => v == "required" || v == "optional",
            },
        }
    }

    /// The hashes object, intended for integrity checks.
    pub fn sha1(&self) -> Option<&str> {
        self.hashes.sha1.as_deref()
    }
}