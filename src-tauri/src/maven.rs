//! Maven artifact coordinate parsing and repository locating.
//!
//! Minecraft / NeoForge metadata uses Maven coordinates of the form
//! `group:artifact:version` (or `group:artifact:version:classifier` with an
//! optional `@extension`). This module knows the two authoritative
//! repositories we need and produces direct URLs for artifacts.

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MavenError {
    #[error("malformed maven coordinate: {0}")]
    Malformed(String),
    #[error("unsupported maven extension: {0}")]
    UnsupportedExt(String),
}

/// A parsed maven coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub group: String,
    pub name: String,
    pub version: String,
    pub classifier: Option<String>,
    pub extension: String,
}

impl Artifact {
    /// Parse `group:artifact:version[:classifier][@ext]`.
    pub fn parse(spec: &str) -> Result<Self, MavenError> {
        let (spec, extension) = match spec.split_once('@') {
            Some((s, ext)) => (s, ext.to_string()),
            None => (spec, "jar".to_string()),
        };
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() < 3 || parts.len() > 4 {
            return Err(MavenError::Malformed(spec.to_string()));
        }
        let classifier = if parts.len() == 4 {
            Some(parts[3].to_string())
        } else {
            None
        };
        Ok(Artifact {
            group: parts[0].to_string(),
            name: parts[1].to_string(),
            version: parts[2].to_string(),
            classifier,
            extension,
        })
    }

    /// The file name of the artifact jar.
    pub fn file_name(&self) -> String {
        match &self.classifier {
            Some(c) => format!("{}-{}-{}.{}", self.name, self.version, c, self.extension),
            None => format!("{}-{}.{}", self.name, self.version, self.extension),
        }
    }

    /// Directory path below a repository root.
    pub fn directory(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.group.replace('.', "/"),
            self.name,
            self.version,
            self.file_name()
        )
    }

    /// Whether the maven XSD namespace indicates the artifact hosts downloads.
    pub fn can_download(&self) -> bool {
        true
    }
}

/// A repository root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repo {
    /// https://libraries.minecraft.net/ (vanilla + some NeoForge artifacts)
    Mojang,
    /// https://maven.neoforged.net/releases/ (NeoForge)
    NeoForged,
    /// https://repo1.maven.org/maven2/ (fallback mirror of Maven Central)
    MavenCentral,
}

impl Repo {
    pub fn root(&self) -> &'static str {
        match self {
            Repo::Mojang => "https://libraries.minecraft.net",
            Repo::NeoForged => "https://maven.neoforged.net/releases",
            Repo::MavenCentral => "https://repo1.maven.org/maven2",
        }
    }

    /// Attempt to resolve a coordinate into (repo, url). Returns the first
    /// repository that we know could host this artifact.
    pub fn locate(&self, art: &Artifact) -> String {
        format!("{}/{}", self.root(), art.directory())
    }
}

/// The libraries referenced by a NeoForge installer / version json may have
/// their own `downloads` field with an explicit URL. If present, use it.
#[derive(Debug, Clone, Deserialize)]
pub struct LibraryDownload {
    pub path: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibraryArtifact {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

/// A library entry in a NeoForge version.json.
#[derive(Debug, Clone, Deserialize)]
pub struct Library {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<Downloads>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Downloads {
    #[serde(default)]
    pub artifact: Option<LibraryArtifact>,
}

impl Library {
    /// Resolve the download URL for this library. Uses explicit `downloads`
    /// when present, else falls back to a repository guess.
    pub fn resolve_url(&self) -> Option<String> {
        if let Some(d) = &self.downloads {
            if let Some(a) = &d.artifact {
                if let Some(url) = &a.url {
                    return Some(url.clone());
                }
            }
        }
        let art = Artifact::parse(&self.name).ok()?;
        // Try the repos in order; older NeoForge libs live on Mojang's repo.
        Some(Repo::NeoForged.locate(&art))
    }

    /// Filename to store the jar under.
    pub fn file_name(&self) -> Option<String> {
        Artifact::parse(&self.name).ok().map(|a| a.file_name())
    }

    /// SHA-1 declared by metadata, if any.
    pub fn expected_sha1(&self) -> Option<String> {
        self.downloads
            .as_ref()?
            .artifact
            .as_ref()?
            .sha1
            .clone()
    }

    /// Expected size declared by metadata, if any.
    pub fn expected_size(&self) -> Option<u64> {
        self.downloads.as_ref()?.artifact.as_ref()?.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_coordinates() {
        let a = Artifact::parse("net.neoforged:neoforge:21.1.233").unwrap();
        assert_eq!(a.group, "net.neoforged");
        assert_eq!(a.name, "neoforge");
        assert_eq!(a.version, "21.1.233");
        assert_eq!(a.file_name(), "neoforge-21.1.233.jar");
        assert_eq!(
            a.directory(),
            "net/neoforged/neoforge/21.1.233/neoforge-21.1.233.jar"
        );
        let c = Artifact::parse("net.neoforged:neoform:1.21.1-20240808.144430:zip@zip")
            .unwrap();
        assert_eq!(c.classifier.as_deref(), Some("zip"));
        assert_eq!(c.extension, "zip");
        assert_eq!(c.file_name(), "neoform-1.21.1-20240808.144430-zip.zip");
    }

    #[test]
    fn repo_locations() {
        let a = Artifact::parse("net.neoforged:neoforge:21.1.233").unwrap();
        assert_eq!(
            Repo::NeoForged.locate(&a),
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/21.1.233/neoforge-21.1.233.jar"
        );
    }
}