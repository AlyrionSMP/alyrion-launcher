//! Java runtime detection, verification, and automated Temurin 21 provisioning.

use crate::game::GameError;
use crate::update::{download_verified, format_bytes, Progress, UpdateError, UpdateStage};
use flate2::read::GzDecoder;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

#[derive(Debug, Clone)]
pub struct JavaInfo {
    pub path: PathBuf,
    pub major: u16,
}

#[derive(Debug, Clone)]
pub struct TemurinDownloadInfo {
    pub url: String,
    pub filename: String,
    pub is_zip: bool,
}

/// Resolve the official Adoptium Temurin 21 JRE download URL for the current OS and CPU architecture.
pub fn temurin_download_info() -> Result<TemurinDownloadInfo, UpdateError> {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return Err(UpdateError::Integrity(format!(
            "unsupported operating system: {}",
            std::env::consts::OS
        )));
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return Err(UpdateError::Integrity(format!(
            "unsupported CPU architecture: {}",
            std::env::consts::ARCH
        )));
    };

    let is_zip = cfg!(target_os = "windows");
    let ext = if is_zip { "zip" } else { "tar.gz" };
    let filename = format!("temurin-21-{os}-{arch}.{ext}");
    let url = format!(
        "https://api.adoptium.net/v3/binary/latest/21/ga/{os}/{arch}/jre/hotspot/normal/eclipse?project=jdk"
    );

    Ok(TemurinDownloadInfo {
        url,
        filename,
        is_zip,
    })
}

/// Candidate `bin/java` (or `java.exe`) paths for the current OS and managed runtime, in priority order.
pub fn java_candidates(base_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let bin_name = if cfg!(target_os = "windows") {
        "java.exe"
    } else {
        "java"
    };

    let runtime_dir = base_dir.join("runtime");
    // Managed runtime paths: direct root, macOS App Bundle layout, or unpacked subfolder.
    out.push(runtime_dir.join("bin").join(bin_name));
    out.push(runtime_dir.join("Contents").join("Home").join("bin").join(bin_name));
    out.push(runtime_dir.join("java").join("bin").join(bin_name));

    if let Ok(entries) = fs::read_dir(&runtime_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                out.push(p.join("bin").join(bin_name));
                out.push(p.join("Contents").join("Home").join("bin").join(bin_name));
            }
        }
    }

    // macOS: `/usr/libexec/java_home` and standard locations.
    #[cfg(target_os = "macos")]
    {
        for ver in ["21", "22", "23", "24"] {
            if let Ok(h) = Command::new("/usr/libexec/java_home").arg("-v").arg(ver).output() {
                if h.status.success() {
                    let home = String::from_utf8_lossy(&h.stdout).trim().to_string();
                    if !home.is_empty() {
                        out.push(Path::new(&home).join("bin").join("java"));
                    }
                }
            }
        }
        if let Ok(h) = Command::new("/usr/libexec/java_home").output() {
            if h.status.success() {
                let home = String::from_utf8_lossy(&h.stdout).trim().to_string();
                if !home.is_empty() {
                    out.push(Path::new(&home).join("bin").join("java"));
                }
            }
        }
        if let Ok(entries) = fs::read_dir("/Library/Java/JavaVirtualMachines") {
            let mut jdks: Vec<_> = entries.flatten().collect();
            jdks.sort_by_key(|e| e.file_name());
            for e in jdks.into_iter().rev() {
                out.push(e.path().join("Contents").join("Home").join("bin").join("java"));
            }
        }
        for p in [
            "/opt/homebrew/opt/openjdk@21/bin/java",
            "/opt/homebrew/opt/openjdk/bin/java",
            "/usr/local/opt/openjdk@21/bin/java",
            "/usr/local/opt/openjdk/bin/java",
        ] {
            out.push(PathBuf::from(p));
        }
    }

    // System PATH lookup (all operating systems).
    if let Some(p) = which(bin_name) {
        out.push(p);
    }

    // Common Linux install locations.
    #[cfg(target_os = "linux")]
    for p in [
        "/usr/lib/jvm/java-21-openjdk-amd64/bin/java",
        "/usr/lib/jvm/temurin-21-jdk-amd64/bin/java",
        "/usr/lib/jvm/java-21-temurin/bin/java",
        "/usr/lib/jvm/java-21-openjdk/bin/java",
        "/usr/lib/jvm/java-21/bin/java",
        "/usr/lib/jvm/default-java/bin/java",
    ] {
        out.push(PathBuf::from(p));
    }

    out
}

/// Locate Java: managed runtime in launcher dir, standard OS paths, or system PATH. Requires Java 21+.
pub fn find_java(base_dir: &Path) -> Result<JavaInfo, GameError> {
    for cand in java_candidates(base_dir) {
        if cand.is_file() {
            if let Some(info) = probe_java(&cand) {
                return Ok(info);
            }
        }
    }
    Err(GameError::JavaNotFound(
        "no Java 21+ runtime found — Temurin 21 will be automatically installed on update/play".into(),
    ))
}

pub fn probe_java(path: &Path) -> Option<JavaInfo> {
    let out = Command::new(path).arg("-version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let major = parse_java_major(&text);
    if major >= 21 {
        Some(JavaInfo {
            path: path.to_path_buf(),
            major,
        })
    } else {
        None
    }
}

pub fn parse_java_major(stderr: &str) -> u16 {
    for line in stderr.lines() {
        let line = line.trim();
        let Some(start) = line.find('"') else { continue };
        let rest = &line[start + 1..];
        let ver = rest.split('"').next().unwrap_or("");
        let parts: Vec<&str> = ver.split('.').collect();
        if parts[0] == "1" && parts.len() > 1 {
            if let Ok(v) = parts[1].parse::<u16>() {
                return v;
            }
        } else if let Ok(v) = parts[0].parse::<u16>() {
            return v;
        }
    }
    0
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn extract_tar_gz(archive_path: &Path, dst: &Path) -> Result<(), UpdateError> {
    let file = fs::File::open(archive_path)?;
    let gz = GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    archive.unpack(dst)?;
    Ok(())
}

fn extract_zip(archive_path: &Path, dst: &Path) -> Result<(), UpdateError> {
    let file = fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    archive.extract(dst)?;
    Ok(())
}

#[cfg(unix)]
fn make_executables_recursive(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                make_executables_recursive(&p);
            } else if let Some(parent) = p.parent() {
                if parent.ends_with("bin") || p.file_name().map(|n| n == "java").unwrap_or(false) {
                    let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o755));
                }
            }
        }
    }
}

pub fn extract_runtime(archive_path: &Path, base_dir: &Path, is_zip: bool) -> Result<(), UpdateError> {
    let staging = base_dir.join(".runtime-staging");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;

    if is_zip {
        extract_zip(archive_path, &staging)?;
    } else {
        extract_tar_gz(archive_path, &staging)?;
    }

    let final_runtime = base_dir.join("runtime");
    let backup = base_dir.join(".runtime-old");
    let _ = fs::remove_dir_all(&backup);
    if final_runtime.exists() {
        fs::rename(&final_runtime, &backup)?;
    }
    if let Err(e) = fs::rename(&staging, &final_runtime) {
        let _ = fs::rename(&backup, &final_runtime);
        return Err(UpdateError::Io(e));
    }
    let _ = fs::remove_dir_all(&backup);

    #[cfg(unix)]
    make_executables_recursive(&final_runtime);

    Ok(())
}

/// Ensure a valid Java 21+ runtime is available. If missing, automatically downloads
/// and installs official Eclipse Temurin 21 for the current OS/architecture.
pub async fn ensure_java(
    client: &reqwest::Client,
    base_dir: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<JavaInfo, UpdateError> {
    if let Ok(info) = find_java(base_dir) {
        return Ok(info);
    }

    progress(Progress {
        stage: UpdateStage::Fetching,
        fraction: 0.0,
        detail: "Preparing Java 21 runtime (Temurin 21)…".into(),
    });

    let download_info = temurin_download_info()?;
    let cache_dir = base_dir.join(".alyrion-cache");
    fs::create_dir_all(&cache_dir)?;
    let archive_path = cache_dir.join(&download_info.filename);

    let mut prog = |done: u64, total: u64| {
        if total > 0 {
            progress(Progress {
                stage: UpdateStage::Fetching,
                fraction: done as f32 / total as f32,
                detail: format!(
                    "Java 21 runtime ({} / {})",
                    format_bytes(done),
                    format_bytes(total)
                ),
            });
        }
        let _ = (done, total);
    };

    download_verified(
        client,
        &download_info.url,
        &archive_path,
        None,
        None,
        cancel,
        &mut prog,
    )
    .await?;

    progress(Progress {
        stage: UpdateStage::Extracting,
        fraction: 0.8,
        detail: "Extracting Java 21 runtime…".into(),
    });

    extract_runtime(&archive_path, base_dir, download_info.is_zip)?;

    let java_info = find_java(base_dir).map_err(|e| {
        UpdateError::Integrity(format!("failed to locate extracted Java 21 runtime: {e}"))
    })?;

    progress(Progress {
        stage: UpdateStage::Extracting,
        fraction: 1.0,
        detail: "Java 21 runtime ready".into(),
    });

    Ok(java_info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temurin_download_info() {
        let info = temurin_download_info().unwrap();
        assert!(info.url.contains("adoptium.net/v3/binary/latest/21/ga"));
        assert!(info.filename.starts_with("temurin-21-"));
    }

    #[test]
    fn test_parse_java_major() {
        assert_eq!(parse_java_major("openjdk version \"21.0.6\" 2025-01-21"), 21);
        assert_eq!(parse_java_major("java version \"21.0.1\" 2023-10-17"), 21);
        assert_eq!(parse_java_major("openjdk version \"17.0.2\" 2022-01-18"), 17);
        assert_eq!(parse_java_major("java version \"1.8.0_351\""), 8);
    }
}
