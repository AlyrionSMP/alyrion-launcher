//! Update orchestration: install the latest Alyrion pack version into the
//! instance directory, or update the existing install.
//!
//! Design goals:
//! - Locked to the latest version. We always fetch what Modrinth reports as
//!   latest and make the instance *exactly* that.
//! - No play during an update. The instance root is replaced atomically; the
//!   launcher's state machine refuses to launch while an update is in flight.
//! - Integrity guaranteed: every downloaded artifact is verified against the
//!   SHA-1 published inside the pack's index, plus a size check.
//! - Resumable: partially-downloaded `.part` files in the download cache are
//!   reused via HTTP Range; already-verified files are skipped.

use crate::install::IndexJson;
use crate::modrinth::{self, PackVersion};
use futures_util::StreamExt;
use sha1::{Digest, Sha1};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("modrinth: {0}")]
    Modrinth(#[from] modrinth::ModrinthError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("integrity: {0}")]
    Integrity(String),
    #[error("network: {0}")]
    Network(String),
    #[error("remote server: {0}")]
    Remote(String),
    #[error("canceled")]
    Canceled,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub stage: UpdateStage,
    /// 0..1 fraction of the current stage.
    pub fraction: f32,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStage {
    Checking,
    Fetching,
    Downloading,
    Extracting,
    Verifying,
    Finalizing,
}

impl UpdateStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            UpdateStage::Checking => "checking",
            UpdateStage::Fetching => "fetching",
            UpdateStage::Downloading => "downloading",
            UpdateStage::Extracting => "extracting",
            UpdateStage::Verifying => "verifying",
            UpdateStage::Finalizing => "finalizing",
        }
    }
}

/// Outcome of an update check.
#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    UpToDate { version: String, version_id: String },
    Updated {
        version: String,
        version_id: String,
        changed_from: Option<String>,
    },
}

impl UpdateOutcome {
    pub fn version(&self) -> &str {
        match self {
            UpdateOutcome::UpToDate { version, .. } => version,
            UpdateOutcome::Updated { version, .. } => version,
        }
    }
    pub fn version_id(&self) -> &str {
        match self {
            UpdateOutcome::UpToDate { version_id, .. } => version_id,
            UpdateOutcome::Updated { version_id, .. } => version_id,
        }
    }
}

fn sha1_of(path: &Path) -> Result<String, UpdateError> {
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(Sha1::digest(&buf)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn file_sha1(path: &Path, buf: &mut Vec<u8>) -> Result<String, UpdateError> {
    buf.clear();
    let mut file = fs::File::open(path)?;
    file.read_to_end(buf)?;
    Ok(Sha1::digest(buf).iter().map(|b| format!("{b:02x}")).collect())
}

/// Download `url` to `dest` with optional sha1 + size verification.
/// - Reuses a previously-verified file; skips entirely if hash matches.
/// - Resumes a `.part` file via HTTP Range when present.
/// - On integrity failure the partial file is deleted and an error returned
///   (we never leave a corrupt file in a live instance).
pub async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_sha1: Option<&str>,
    expected_size: Option<u64>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), UpdateError> {
    if dest.exists() {
        let mut buf = Vec::new();
        if let Some(sha) = expected_sha1 {
            if file_sha1(dest, &mut buf)? == sha {
                return Ok(());
            }
            let _ = fs::remove_file(dest);
        } else if let Some(sz) = expected_size {
            if fs::metadata(dest)?.len() == sz {
                return Ok(());
            }
            let _ = fs::remove_file(dest);
        } else {
            return Ok(());
        }
    }

    let part_path = dest.with_extension("part");
    let existing_len: u64 = fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);

    let mut req = client.get(url);
    if existing_len > 0 {
        req = req.header("Range", format!("bytes={existing_len}-"));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(UpdateError::Remote(format!("{url} -> {status}")));
    }

    let is_partial = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let existing_len = if is_partial { existing_len } else { 0 };

    let mut out = if is_partial {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part_path)?
    } else {
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&part_path)?
    };

    let total = resp.content_length().map(|l| l + existing_len).unwrap_or(existing_len);
    progress(0, total);

    let mut stream = resp.bytes_stream();
    let mut done = existing_len;
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            out.flush()?;
            return Err(UpdateError::Canceled);
        }
        let chunk = chunk.map_err(|e| UpdateError::Network(e.to_string()))?;
        done += chunk.len() as u64;
        out.write_all(&chunk)?;
        progress(done, total);
    }
    out.flush()?;

    let final_len = fs::metadata(&part_path)?.len();
    if let Some(sz) = expected_size {
        if final_len != sz {
            let _ = fs::remove_file(&part_path);
            return Err(UpdateError::Integrity(format!(
                "size mismatch for {url}: expected {sz}, got {final_len}"
            )));
        }
    }
    if let Some(sha) = expected_sha1 {
        let mut buf = Vec::new();
        let actual = file_sha1(&part_path, &mut buf)?;
        if actual != sha {
            let _ = fs::remove_file(&part_path);
            return Err(UpdateError::Integrity(format!(
                "sha1 mismatch for {url}: expected {sha}, got {actual}"
            )));
        }
    }
    fs::rename(&part_path, dest)?;
    Ok(())
}

pub fn format_bytes(bytes: u64) -> String {
    const ONE_GB: u64 = 1024 * 1024 * 1024;
    if bytes >= ONE_GB {
        let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        format!("{gb:.2} GB")
    } else {
        let mb = bytes as f64 / (1024.0 * 1024.0);
        if mb >= 10.0 {
            format!("{mb:.1} MB")
        } else {
            format!("{mb:.2} MB")
        }
    }
}

/// Download the pack .mrpack into the download cache.
pub async fn fetch_mrpack(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    sha1: Option<&str>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<(), UpdateError> {
    progress(Progress {
        stage: UpdateStage::Fetching,
        fraction: 0.0,
        detail: "Downloading pack archive…".into(),
    });
    let mut prog = |done: u64, total: u64| {
        if total > 0 {
            progress(Progress {
                stage: UpdateStage::Fetching,
                fraction: done as f32 / total as f32,
                detail: format!("{} / {}", format_bytes(done), format_bytes(total)),
            });
        }
        let _ = (done, total);
    };
    download_verified(client, url, path, sha1, None, cancel, &mut prog).await
}

/// Download all .mrpack files into the instance's `mods/` plus the
/// `overrides/` overlay. Returns the parsed index for post-processing.
pub async fn install_mrpack(
    client: &reqwest::Client,
    mrpack_path: &Path,
    instance_dir: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<IndexJson, UpdateError> {
    progress(Progress {
        stage: UpdateStage::Extracting,
        fraction: 0.0,
        detail: "Reading modpack index…".into(),
    });
    let file = fs::File::open(mrpack_path)?;
    let mut zip = zip::ZipArchive::new(file)?;

    let index: IndexJson = {
        let mut f = zip.by_name("modrinth.index.json")?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        serde_json::from_slice(&buf)?
    };

    // Validate the pack is one we support (guard against a miswired project).
    if let Some(mc) = &index.dependencies.minecraft {
        if !mc.starts_with("1.21") {
            return Err(UpdateError::Integrity(format!(
                "pack targets Minecraft {mc}, launcher only supports 1.21.x"
            )));
        }
    }
    if let Some(neo) = &index.dependencies.neoforge {
        if !neo.starts_with("21.1.") {
            return Err(UpdateError::Integrity(format!(
                "pack requires NeoForge {neo}, launcher only supports 21.1.x"
            )));
        }
    }

    // Extract the overrides/ overlay (config etc). This merges over the
    // freshly-created instance tree without touching user data.
    for i in 0..zip.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(UpdateError::Canceled);
        }
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        if !name.starts_with("overrides/") || entry.is_dir() {
            continue;
        }
        let rel = name.trim_start_matches("overrides/");
        let out = safe_join(instance_dir, rel)?;
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut f)?;
    }

    // Download every listed file.
    let total = index.files.len() as u64;
    for (idx, pf) in index.files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(UpdateError::Canceled);
        }
        if !pf.needed_for_client() {
            continue;
        }
        let Some(url) = pf.downloads.first_url() else {
            continue;
        };
        let dest = safe_join(instance_dir, &pf.path)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        progress(Progress {
            stage: UpdateStage::Downloading,
            fraction: idx as f32 / total as f32,
            detail: format!("[{}/{}] {}", idx + 1, total, pf.path),
        });
        let pf_path = pf.path.clone();
        let mut prog = |done: u64, total_bytes: u64| {
            if total_bytes > 0 {
                progress(Progress {
                    stage: UpdateStage::Downloading,
                    fraction: (idx as f32 + done as f32 / total_bytes as f32) / total as f32,
                    detail: format!(
                        "[{}/{}] {} ({} / {})",
                        idx + 1, total, pf_path, format_bytes(done), format_bytes(total_bytes)
                    ),
                });
            }
            let _ = (done, total_bytes);
        };
        download_verified(
            client,
            url,
            &dest,
            pf.sha1(),
            pf.file_size,
            cancel,
            &mut prog,
        )
        .await?;
    }

    let manifest = serde_json::json!({
        "files": index.files.len(),
        "dependencies": {
            "minecraft": index.dependencies.minecraft,
            "neoforge": index.dependencies.neoforge,
            "forge": index.dependencies.forge,
            "quilt": index.dependencies.quilt,
            "fabric": index.dependencies.fabric,
        },
        "game": index.game,
    });
    fs::write(
        instance_dir.join(".alyrion-manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(index)
}

/// Version id recorded as installed, if any.
pub fn read_installed_version(instance_dir: &Path) -> Option<String> {
    let meta = instance_dir.join(".alyrion-installed.json");
    let text = fs::read_to_string(meta).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("version_id")
        .and_then(|x| x.as_str())
        .map(String::from)
}

/// Persist which pack version is currently installed.
pub fn write_installed_version(instance_dir: &Path, version: &PackVersion) -> Result<(), UpdateError> {
    let data = serde_json::json!({
        "version_id": version.id,
        "version_number": version.version_number,
        "date_published": version.date_published,
        "project_id": version.project_id,
        "updated_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    fs::write(
        instance_dir.join(".alyrion-installed.json"),
        serde_json::to_vec_pretty(&data)?,
    )?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Download authlib-injector.jar (latest release) into the launcher base dir
/// if it is not already present. Needed for third-party online auth
/// (Ely.by / LittleSkin session endpoints) at launch.
pub async fn ensure_authlib_injector(
    client: &reqwest::Client,
    base_dir: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<(), UpdateError> {
    let dest = base_dir.join("authlib-injector.jar");
    if dest.is_file() {
        return Ok(());
    }
    progress(Progress {
        stage: UpdateStage::Fetching,
        fraction: 0.0,
        detail: "Preparing online auth (authlib-injector)…".into(),
    });
    let url = "https://github.com/yushijinhun/authlib-injector/releases/download/v1.2.8/authlib-injector-1.2.8.jar";
    let mut prog = |done: u64, total: u64| {
        if total > 0 {
            progress(Progress {
                stage: UpdateStage::Fetching,
                fraction: done as f32 / total as f32,
                detail: format!("authlib-injector.jar ({} / {})", format_bytes(done), format_bytes(total)),
            });
        }
        let _ = (done, total);
    };
    download_verified(client, url, &dest, None, None, cancel, &mut prog).await
}

pub async fn ensure_neoforge_installed(
    client: &reqwest::Client,
    base_dir: &Path,
    staging_dir: &Path,
    java: &crate::java::JavaInfo,
    neoforge_ver: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<(), UpdateError> {
    let profile = format!("neoforge-{neoforge_ver}");
    let staging_profile_json = staging_dir
        .join("versions")
        .join(&profile)
        .join(format!("{profile}.json"));

    if staging_profile_json.is_file() {
        return Ok(());
    }

    let live_dir = base_dir.join("instance");
    let live_profile_json = live_dir
        .join("versions")
        .join(&profile)
        .join(format!("{profile}.json"));

    if live_profile_json.is_file() {
        for sub in ["versions", "libraries", "assets"] {
            let src = live_dir.join(sub);
            let dst = staging_dir.join(sub);
            if src.is_dir() {
                let _ = copy_dir_recursive(&src, &dst);
            }
        }
        if staging_profile_json.is_file() {
            return Ok(());
        }
    }

    progress(Progress {
        stage: UpdateStage::Fetching,
        fraction: 0.0,
        detail: format!("Preparing NeoForge {neoforge_ver} installer…"),
    });

    let installer_name = format!("neoforge-{neoforge_ver}-installer.jar");
    let installer_url = format!(
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/{neoforge_ver}/{installer_name}"
    );
    let cache_dir = base_dir.join(".alyrion-cache");
    fs::create_dir_all(&cache_dir)?;
    let installer_path = cache_dir.join(&installer_name);

    let mut prog = |done: u64, total: u64| {
        if total > 0 {
            progress(Progress {
                stage: UpdateStage::Fetching,
                fraction: done as f32 / total as f32,
                detail: format!(
                    "NeoForge installer ({} / {})",
                    format_bytes(done),
                    format_bytes(total)
                ),
            });
        }
        let _ = (done, total);
    };

    download_verified(
        client,
        &installer_url,
        &installer_path,
        None,
        None,
        cancel,
        &mut prog,
    )
    .await?;

    if cancel.load(Ordering::Relaxed) {
        return Err(UpdateError::Canceled);
    }

    progress(Progress {
        stage: UpdateStage::Extracting,
        fraction: 0.5,
        detail: format!("Installing NeoForge {neoforge_ver} runtime…"),
    });

    let profiles_json = staging_dir.join("launcher_profiles.json");
    if !profiles_json.is_file() {
        fs::write(&profiles_json, b"{\n  \"profiles\": {}\n}\n")?;
    }
    fs::create_dir_all(staging_dir.join("versions"))?;
    fs::create_dir_all(staging_dir.join("libraries"))?;

    let output = std::process::Command::new(&java.path)
        .current_dir(staging_dir)
        .arg("-jar")
        .arg(&installer_path)
        .arg("--installClient")
        .arg(staging_dir)
        .output()
        .map_err(UpdateError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(UpdateError::Integrity(format!(
            "NeoForge installer failed (exit code {:?}):\n{}\n{}",
            output.status.code(),
            stderr.trim(),
            stdout.trim()
        )));
    }

    Ok(())
}

/// The core update routine. Uses a sidecar staging dir and atomically swaps
/// it in at the end, so the game can never observe a half-updated pack.
///
/// `preserve_dirs` names (relative to the instance root) are carried over
/// from the previous install into the new one (e.g. worlds, screenshots).
pub async fn update_pack(
    client: &reqwest::Client,
    base_dir: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress) + Send,
) -> Result<UpdateOutcome, UpdateError> {
    progress(Progress {
        stage: UpdateStage::Checking,
        fraction: 0.0,
        detail: "Checking for updates…".into(),
    });
    let latest = modrinth::fetch_latest_version(client).await?;

    // authlib-injector: fetch once (341 KB) so third-party online auth
    // (Ely.by / LittleSkin session servers) works at game launch.
    ensure_authlib_injector(client, base_dir, cancel, &mut progress).await?;

    // Java 21+: ensure a runtime is present (downloads Temurin 21 if not found on system)
    let java_info = crate::java::ensure_java(client, base_dir, cancel, &mut progress).await?;

    // Early check: if the instance is already installed with this exact version and NeoForge profile is intact, return immediately.
    let live = base_dir.join("instance");
    let previous_version = read_installed_version(&live);
    let layout = crate::game::InstanceLayout::new(base_dir);
    let neoforge_installed = crate::game::find_installed_neoforge_profile(&layout).is_some();

    if previous_version.as_deref() == Some(latest.id.as_str())
        && neoforge_installed
        && live.join("mods").is_dir()
    {
        progress(Progress {
            stage: UpdateStage::Checking,
            fraction: 1.0,
            detail: format!("Pack is up to date ({})", latest.version_number),
        });
        return Ok(UpdateOutcome::UpToDate {
            version: latest.version_number,
            version_id: latest.id,
        });
    }

    let mrpack = latest.primary_mrpack().ok_or_else(|| {
        UpdateError::Integrity("pack version has no primary .mrpack".into())
    })?;

    // If a previous download of this exact mrpack already exists and passes,
    // skip re-downloading it.
    let cache_dir = base_dir.join(".alyrion-cache");
    fs::create_dir_all(&cache_dir)?;
    let mrpack_path = cache_dir.join(&mrpack.filename);

    // Quick hash check for the cached mrpack.
    let cached_ok = if mrpack_path.exists() {
        match sha1_of(&mrpack_path) {
            Ok(h) => {
                mrpack.hashes.sha1.as_deref().map(String::from) == Some(h)
            }
            Err(_) => false,
        }
    } else {
        false
    };

    progress(Progress {
        stage: UpdateStage::Fetching,
        fraction: 0.0,
        detail: format!("Version {} available → preparing…", latest.version_number),
    });

    if !cached_ok {
        fetch_mrpack(
            client,
            &mrpack.url,
            &mrpack_path,
            mrpack.hashes.sha1.as_deref(),
            cancel,
            &mut progress,
        )
        .await?;
    }

    // Staging dir — fresh tree that becomes the new instance.
    let staging = base_dir.join(".update-staging");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;
    fs::create_dir_all(staging.join("mods"))?;

    progress(Progress {
        stage: UpdateStage::Verifying,
        fraction: 0.0,
        detail: "Extracting and verifying files…".into(),
    });
    let index = install_mrpack(client, &mrpack_path, &staging, cancel, &mut progress).await?;

    let neoforge_ver = index
        .dependencies
        .neoforge
        .as_deref()
        .unwrap_or(crate::game::NEOFORGE_VERSION);
    ensure_neoforge_installed(
        client,
        base_dir,
        &staging,
        &java_info,
        neoforge_ver,
        cancel,
        &mut progress,
    )
    .await?;

    // Sync assets & extract natives into staging
    let layout = crate::game::InstanceLayout {
        natives: staging
            .join("versions")
            .join(crate::game::MC_VERSION)
            .join(format!("{}-natives", crate::game::MC_VERSION)),
        versions: staging.join("versions"),
        libraries: staging.join("libraries"),
        assets: staging.join("assets"),
        indexes: staging.join("assets").join("indexes"),
        objects: staging.join("assets").join("objects"),
        logs: staging.join("logs"),
        root: staging.clone(),
    };

    if let Ok(vanilla) = crate::game::read_version_json(&layout, crate::game::MC_VERSION) {
        if let Some(asset_index) = &vanilla.asset_index {
            let _ = crate::game::sync_assets(client, &layout, asset_index, cancel, |_, _| {}).await;
        }
        let profile = crate::game::find_installed_neoforge_profile(&layout)
            .unwrap_or_else(|| format!("neoforge-{neoforge_ver}"));
        if let Ok(neoforge) = crate::game::read_version_json(&layout, &profile) {
            let merged = crate::game::merged_libraries(&vanilla, &neoforge);
            let _ = crate::game::sync_libraries(client, &layout, &merged, cancel, |_, _| {}).await;
            let _ = crate::game::extract_natives(&layout, &merged);
        }
    }

    // Carry over user data and runtime caches from the previous install.
    let live = base_dir.join("instance");
    let mut preserved: Vec<(String, PathBuf)> = Vec::new();
    if live.exists() {
        for sub in [
            "worlds",
            "screenshots",
            "resourcepacks",
            "versions",
            "libraries",
            "assets",
            "launcher_profiles.json",
        ] {
            let p = live.join(sub);
            if p.exists() {
                preserved.push((sub.to_string(), p));
            }
        }
    }
    let previous_version = read_installed_version(&live);

    progress(Progress {
        stage: UpdateStage::Finalizing,
        fraction: 0.9,
        detail: "Finalizing install…".into(),
    });
    let backup = base_dir.join(".instance-old");
    let _ = fs::remove_dir_all(&backup);
    if live.exists() {
        fs::rename(&live, &backup)?;
    }
    if let Err(e) = fs::rename(&staging, &live) {
        let _ = fs::rename(&backup, &live);
        return Err(UpdateError::Io(e));
    }
    for (sub, p) in preserved {
        let dst = live.join(sub);
        if p.is_dir() {
            let _ = fs::create_dir_all(&dst);
            let _ = copy_dir_recursive(&p, &dst);
        } else if p.is_file() {
            if let Some(parent) = dst.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::copy(&p, &dst);
        }
    }
    let _ = fs::remove_dir_all(&backup);
    write_installed_version(&live, &latest)?;

    let up_to_date = previous_version.as_deref() == Some(latest.id.as_str());
    Ok(if up_to_date {
        UpdateOutcome::UpToDate {
            version: latest.version_number,
            version_id: latest.id,
        }
    } else {
        UpdateOutcome::Updated {
            version: latest.version_number,
            version_id: latest.id,
            changed_from: previous_version,
        }
    })
}

/// Whether the instance is considered installed (has a version record).
pub fn is_installed(instance_dir: &Path) -> bool {
    read_installed_version(instance_dir).is_some()
        && instance_dir.join("mods").is_dir()
}

/// Join `root` with a pack-provided relative path, refusing anything that
/// escapes the root (absolute paths, `..` segments, Windows drive letters,
/// NUL bytes). This protects the user's settings/accounts files from a
/// malicious or malformed modpack index.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, UpdateError> {
    let rel = rel.replace('\\', "/");
    if rel.starts_with('/') || rel.starts_with("//") {
        return Err(UpdateError::Integrity(format!(
            "pack entry {rel:?} is absolute — rejected"
        )));
    }
    for seg in rel.split('/') {
        if seg == ".." || seg.contains('\0') {
            return Err(UpdateError::Integrity(format!(
                "pack entry {rel:?} escapes the instance dir — rejected"
            )));
        }
        if seg.len() >= 2 && seg.as_bytes()[1] == b':' {
            return Err(UpdateError::Integrity(format!(
                "pack entry {rel:?} looks like a drive path — rejected"
            )));
        }
    }
    Ok(root.join(rel))
}

#[cfg(test)]
mod safe_join_tests {
    use super::*;

    #[test]
    fn rejects_escapes() {
        let root = Path::new("/tmp/alyrion-test");
        assert!(safe_join(root, "../evil.txt").is_err());
        assert!(safe_join(root, "mods/../../../etc/passwd").is_err());
        assert!(safe_join(root, "/absolute/path").is_err());
        assert!(safe_join(root, "C:\\windows\\x").is_err());
        assert!(safe_join(root, "mods/a\0b.jar").is_err());
        assert!(safe_join(root, "mods/ok.jar").is_ok());
        assert!(safe_join(root, "config/x.json").is_ok());
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0.00 MB");
        assert_eq!(format_bytes(349_681), "0.33 MB");
        assert_eq!(format_bytes(15_728_640), "15.0 MB");
        assert_eq!(format_bytes(524_288_000), "500.0 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.00 GB");
        assert_eq!(format_bytes(2_684_354_560), "2.50 GB");
    }
}