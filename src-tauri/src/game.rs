//! Game bootstrap: find a Java runtime, merge the vanilla + NeoForge launch
//! profiles, sync assets + natives, build the classpath and spawn the game.
//!
//! The NeoForge official installer has already produced inside the instance:
//! - `versions/1.21.1/1.21.1.json` + `1.21.1.jar`      (vanilla profile)
//! - `versions/neoforge-21.1.233/neoforge-21.1.233.json` (merged NeoForge profile,
//!   `inherits_from: 1.21.1`)
//! - `libraries/**` — the full runtime classpath (patched client included)
//!
//! Launch semantics mirror the official launcher: merged libraries, vanilla
//! argument templates with token substitution, plus the NeoForge FML args.

use crate::maven::Artifact;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use thiserror::Error;

pub const MC_VERSION: &str = "1.21.1";
pub const NEOFORGE_VERSION: &str = "21.1.233";
pub const NEOFORGE_PROFILE: &str = "neoforge-21.1.233";

#[derive(Debug, Error)]
pub enum GameError {
    #[error("java not found: {0}")]
    JavaNotFound(String),
    #[error("instance not installed yet")]
    NotInstalled,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("maven: {0}")]
    Maven(#[from] crate::maven::MavenError),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("asset sync required: {0}")]
    Assets(String),
}

pub use crate::java::{find_java, parse_java_major, JavaInfo};

/// A single entry of a version.json `libraries` array.
#[derive(Debug, Clone, Deserialize)]
pub struct VerLibrary {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<VerDownloads>,
    /// One or more rules; if any rule blocks the current OS, skip the lib.
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub natives: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerDownloads {
    #[serde(default)]
    pub artifact: Option<VerArtifact>,
    #[serde(default)]
    pub classifiers: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerArtifact {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub action: String,
    #[serde(default)]
    pub os: Option<OsMatch>,
    #[serde(default)]
    pub features: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OsMatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
}

fn evaluate_rules(rules: &[Rule]) -> bool {
    if rules.is_empty() {
        return true;
    }
    let os = current_os_name();
    let arch = current_os_arch();
    // If there is any 'allow' rule without custom features, default is false (opt-in).
    // If all rules are 'disallow', default is true (opt-out).
    let mut decision = !rules.iter().any(|r| r.action == "allow" && r.features.is_none());
    for r in rules {
        if r.features.is_some() {
            continue;
        }
        let matched = match &r.os {
            Some(osm) => {
                let os_ok = osm.name.as_deref().map(|n| n == os).unwrap_or(true);
                let arch_ok = osm.arch.as_deref().map(|a| a == arch).unwrap_or(true);
                os_ok && arch_ok
            }
            None => true,
        };
        if matched {
            decision = r.action == "allow";
        }
    }
    decision
}

impl VerLibrary {
    /// Whether this library applies to the current OS.
    pub fn applies_to_current_os(&self) -> bool {
        evaluate_rules(&self.rules)
    }

    /// Artifact download spec for the current OS (handles natives classifier).
    pub fn artifact_for_os(&self) -> Option<VerArtifact> {
        let d = self.downloads.as_ref()?;
        // Natives: pick the classifier matching our OS.
        if let Some(classifiers) = &d.classifiers {
            let key = natives_key();
            if let Some(v) = classifiers.get(&key) {
                return serde_json::from_value(v.clone()).ok();
            }
            return None;
        }
        d.artifact.clone()
    }

    /// True if this library is a natives (to-extract) lib.
    pub fn is_natives(&self) -> bool {
        self.natives.is_some() && self.downloads.as_ref().and_then(|d| d.classifiers.as_ref()).is_some()
    }
}

fn current_os_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

fn current_os_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86"
    }
}

fn natives_key() -> String {
    let os = current_os_name();
    let arch = current_os_arch();
    let os_key = match os {
        "windows" => "natives-windows",
        "osx" => "natives-osx",
        _ => "natives-linux",
    };
    if arch == "aarch64" {
        format!("{os_key}-arm64")
    } else {
        os_key.to_string()
    }
}

/// Minecraft version JSON (used for vanilla 1.21.1.json).
#[derive(Debug, Clone, Deserialize)]
pub struct McVersionJson {
    pub id: String,
    #[serde(rename = "mainClass")]
    #[serde(default)]
    pub main_class: Option<String>,
    #[serde(rename = "inheritsFrom")]
    #[serde(default)]
    pub inherits_from: Option<String>,
    #[serde(default)]
    pub arguments: Option<ArgumentsJson>,
    #[serde(default)]
    pub libraries: Vec<VerLibrary>,
    #[serde(rename = "assetIndex")]
    #[serde(default)]
    pub asset_index: Option<AssetIndex>,
    #[serde(default)]
    pub logging: Option<serde_json::Value>,
    #[serde(default)]
    pub java_version: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ArgumentsJson {
    #[serde(default)]
    pub game: Vec<serde_json::Value>,
    #[serde(default)]
    pub jvm: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetIndex {
    pub id: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub total_size: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
}

/// Location of everything the runtime needs in the instance dir.
#[derive(Debug, Clone)]
pub struct InstanceLayout {
    pub root: PathBuf,
    pub libraries: PathBuf,
    pub versions: PathBuf,
    pub assets: PathBuf,
    pub indexes: PathBuf,
    pub objects: PathBuf,
    pub natives: PathBuf,
    pub logs: PathBuf,
}

impl InstanceLayout {
    pub fn new(base: &Path) -> Self {
        let root = base.join("instance");
        InstanceLayout {
            natives: root.join("versions").join("1.21.1").join("1.21.1-natives"),
            versions: root.join("versions"),
            libraries: root.join("libraries"),
            assets: root.join("assets"),
            indexes: root.join("assets").join("indexes"),
            objects: root.join("assets").join("objects"),
            logs: root.join("logs"),
            root,
        }
    }
}

/// Read a version profile json from the installed instance.
pub fn read_version_json(layout: &InstanceLayout, profile: &str) -> Result<McVersionJson, GameError> {
    let p = layout.versions.join(profile).join(format!("{profile}.json"));
    let text = fs::read_to_string(&p)?;
    Ok(serde_json::from_str(&text)?)
}

/// All libraries that apply (merged across profiles), in launch order:
/// vanilla first (deduped), then NeoForge additions.
pub fn merged_libraries(
    vanilla: &McVersionJson,
    neoforge: &McVersionJson,
) -> Vec<(VerLibrary, Option<VerArtifact>, bool)> {
    let mut out: Vec<(VerLibrary, Option<VerArtifact>, bool)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push = |lib: &VerLibrary, out: &mut Vec<_>| {
        if !lib.applies_to_current_os() {
            return;
        }
        let is_native = lib.is_natives();
        let art = lib.artifact_for_os();
        let key = art.as_ref().and_then(|a| a.url.clone()).unwrap_or_else(|| lib.name.clone());
        if !is_native && !seen.insert(key.clone()) {
            return;
        }
        out.push((lib.clone(), art, is_native));
    };

    for lib in &vanilla.libraries {
        push(lib, &mut out);
    }
    for lib in &neoforge.libraries {
        push(lib, &mut out);
    }
    out
}

/// Where a library's jar should live inside the instance.
pub fn library_local_path(layout: &InstanceLayout, lib: &VerLibrary, art: &VerArtifact) -> PathBuf {
    if let Some(path) = &art.path {
        layout.libraries.join(path)
    } else {
        match Artifact::parse(&lib.name) {
            Ok(a2) => layout.libraries.join(a2.directory()),
            Err(_) => layout.libraries.join(format!("{}.jar", lib.name.replace(':', "-"))),
        }
    }
}

/// Sync the asset index + all object files. Resumable, verified by sha1.
pub async fn sync_assets(
    client: &reqwest::Client,
    layout: &InstanceLayout,
    index_json: &AssetIndex,
    cancel: &std::sync::atomic::AtomicBool,
    mut progress: impl FnMut(f32, &str) + Send,
) -> Result<(), GameError> {
    fs::create_dir_all(&layout.indexes)?;
    fs::create_dir_all(&layout.objects)?;

    // 1. Fetch the index file.
    let idx_path = layout.indexes.join(format!("{}.json", index_json.id));
    if !idx_path.is_file() {
        let url = index_json
            .url
            .clone()
            .ok_or_else(|| GameError::Assets("asset index has no url".into()))?;
        progress(0.0, "Downloading asset index…");
        crate::update::download_verified(
            client,
            &url,
            &idx_path,
            index_json.sha1.as_deref(),
            index_json.size,
            cancel,
            &mut |_, _| {},
        )
        .await
        .map_err(|e| GameError::Assets(e.to_string()))?;
    }
    let text = fs::read_to_string(&idx_path)?;
    let idx: serde_json::Value = serde_json::from_str(&text)?;
    let objects = idx
        .get("objects")
        .and_then(|o| o.as_object())
        .ok_or_else(|| GameError::Assets("asset index has no objects".into()))?;

    // 2. Download each object (hashed path == sha1 prefix).
    let total = objects.len() as f32;
    let mut done = 0usize;
    for (name, obj) in objects {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let hash = obj
            .get("hash")
            .and_then(|h| h.as_str())
            .unwrap_or_default()
            .to_string();
        if hash.len() < 2 {
            continue;
        }
        let size = obj.get("size").and_then(|s| s.as_u64());
        let dest = layout.objects.join(&hash[0..2]).join(&hash);
        if dest.exists() {
            done += 1;
            continue;
        }
        if done % 32 == 0 {
            progress(done as f32 / total, &format!("Assets: {name}"));
        }
        let url = format!(
            "https://resources.download.minecraft.net/{}/{}",
            &hash[0..2],
            &hash
        );
        fs::create_dir_all(dest.parent().unwrap())?;
        crate::update::download_verified(client, &url, &dest, Some(&hash), size, cancel, &mut |_, _| {})
            .await
            .map_err(|e| GameError::Assets(format!("{name}: {e}")))?;
        done += 1;
    }
    progress(1.0, "Assets ready");
    Ok(())
}

/// Sync all missing library jars for the current OS. Resumable, verified by sha1 when present.
pub async fn sync_libraries(
    client: &reqwest::Client,
    layout: &InstanceLayout,
    libs: &[(VerLibrary, Option<VerArtifact>, bool)],
    cancel: &std::sync::atomic::AtomicBool,
    mut progress: impl FnMut(f32, &str) + Send,
) -> Result<(), GameError> {
    fs::create_dir_all(&layout.libraries)?;
    let total = libs.len() as f32;
    for (i, (lib, art, _is_native)) in libs.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let (dest, url, sha1, size) = if let Some(art) = art {
            let dest = library_local_path(layout, lib, art);
            let url = art.url.clone().unwrap_or_else(|| {
                if let Some(p) = &art.path {
                    format!("https://libraries.minecraft.net/{p}")
                } else if let Ok(a2) = Artifact::parse(&lib.name) {
                    format!("https://libraries.minecraft.net/{}", a2.directory())
                } else {
                    String::new()
                }
            });
            (dest, url, art.sha1.clone(), art.size)
        } else if let Ok(a2) = Artifact::parse(&lib.name) {
            let dest = layout.libraries.join(a2.directory());
            let url = format!("https://libraries.minecraft.net/{}", a2.directory());
            (dest, url, None, None)
        } else {
            continue;
        };

        if url.is_empty() || dest.is_file() {
            continue;
        }

        if let Some(parent) = dest.parent() {
            let _ = fs::create_dir_all(parent);
        }

        progress(i as f32 / total, &format!("Library: {}", lib.name));

        let res = crate::update::download_verified(
            client,
            &url,
            &dest,
            sha1.as_deref(),
            size,
            cancel,
            &mut |_, _| {},
        )
        .await;

        if res.is_err() {
            if let Ok(a2) = Artifact::parse(&lib.name) {
                let fallback = format!("https://libraries.minecraft.net/{}", a2.directory());
                if fallback != url {
                    let _ = crate::update::download_verified(
                        client,
                        &fallback,
                        &dest,
                        sha1.as_deref(),
                        size,
                        cancel,
                        &mut |_, _| {},
                    )
                    .await;
                }
            }
        }
    }
    Ok(())
}

/// Extract natives jars (lwjgl etc.) into the natives dir.
pub fn extract_natives(layout: &InstanceLayout, libs: &[(VerLibrary, Option<VerArtifact>, bool)]) -> Result<(), GameError> {
    let natives_dir = &layout.natives;
    if natives_dir.exists() {
        fs::remove_dir_all(natives_dir)?;
    }
    fs::create_dir_all(natives_dir)?;
    let mut any = false;
    for (lib, art, is_native) in libs {
        if !is_native {
            continue;
        }
        let Some(art) = art else { continue };
        let jar = library_local_path(layout, lib, art);
        if !jar.is_file() {
            continue;
        }
        any = true;
        // Copy the .so/.dll/.dylib files out of the jar.
        let file = fs::File::open(&jar)?;
        let mut z = zip::ZipArchive::new(file)?;
        for i in 0..z.len() {
            let mut entry = z.by_index(i)?;
            let name = entry.name().to_string();
            // Only top-level native files (no dirs, no class/jar files).
            if entry.is_dir() || name.contains('/') || !(name.ends_with(".so") || name.ends_with(".dylib") || name.ends_with(".dll")) {
                continue;
            }
            let out = natives_dir.join(&name);
            let mut f = fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut f)?;
        }
    }
    let _ = any;
    Ok(())
}

/// A resolved session (account) for launching.
#[derive(Debug, Clone)]
pub struct Session {
    pub username: String,
    pub uuid: String,
    pub access_token: String,
    pub user_type: String,
    /// When set, `authlib-injector` is added as a javaagent with this
    /// Mojang-API-compatible base URL (Ely.by: `https://authserver.ely.by`,
    /// LittleSkin: their Yggdrasil server). Offline sessions leave it None.
    pub authserver_url: Option<String>,
}

/// Everything needed to spawn the game.
pub struct LaunchSpec {
    pub java: JavaInfo,
    pub cwd: PathBuf,
    pub args: Vec<String>,
    pub envs: Vec<(String, String)>,
    pub log_path: PathBuf,
}

fn substitute(text: &str, vars: &[(&str, &str)]) -> String {
    let mut out = text.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("${{{k}}}"), v);
    }
    out
}

fn evaluate_arguments(args: &[serde_json::Value]) -> Vec<String> {
    let mut out = Vec::new();
    for item in args {
        if let Some(s) = item.as_str() {
            out.push(s.to_string());
        } else if let Some(obj) = item.as_object() {
            let applies = if let Some(rules_val) = obj.get("rules") {
                if let Ok(rules) = serde_json::from_value::<Vec<Rule>>(rules_val.clone()) {
                    evaluate_rules(&rules)
                } else {
                    true
                }
            } else {
                true
            };
            if applies {
                if let Some(val) = obj.get("value") {
                    if let Some(s) = val.as_str() {
                        out.push(s.to_string());
                    } else if let Some(arr) = val.as_array() {
                        for v in arr {
                            if let Some(s) = v.as_str() {
                                out.push(s.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

pub fn find_installed_neoforge_profile(layout: &InstanceLayout) -> Option<String> {
    if layout.versions.join(NEOFORGE_PROFILE).join(format!("{NEOFORGE_PROFILE}.json")).is_file() {
        return Some(NEOFORGE_PROFILE.to_string());
    }
    if let Ok(entries) = fs::read_dir(&layout.versions) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("neoforge") && entry.path().join(format!("{name}.json")).is_file() {
                return Some(name);
            }
        }
    }
    None
}

/// Build the complete, ready-to-spawn launch command.
///
/// Steps:
/// 1. read vanilla + neoforge profiles
/// 2. sync assets + extract natives (must be done by caller beforehand;
///    `assets_ready` flips when done)
/// 3. assemble merged classpath
/// 4. merge argument templates and substitute session tokens
pub fn build_launch_spec(
    base_dir: &Path,
    java: &JavaInfo,
    session: &Session,
    mem_mb: u32,
    custom_jvm_args: &str,
) -> Result<LaunchSpec, GameError> {
    let layout = InstanceLayout::new(base_dir);
    let profile = find_installed_neoforge_profile(&layout).ok_or(GameError::NotInstalled)?;
    let vanilla = read_version_json(&layout, MC_VERSION)?;
    let neoforge = read_version_json(&layout, &profile)?;

    let merged = merged_libraries(&vanilla, &neoforge);

    // Classpath: every non-native jar that exists on disk, in order.
    // (Artifacts were placed by the NeoForge installer during pack install.)
    let mut cp: Vec<PathBuf> = Vec::new();
    for (lib, art, is_native) in &merged {
        if *is_native {
            continue;
        }
        let Some(art) = art else { continue };
        let p = library_local_path(&layout, lib, art);
        if p.is_file() {
            cp.push(p);
        }
    }
    // The patched NeoForge client must be on the classpath; it is listed under
    // the neoforge profile (path libraries/net/neoforged/neoforge/...).
    let sep = if cfg!(windows) { ";" } else { ":" };

    let natives_dir = layout.natives.to_string_lossy().to_string();
    let game_dir = layout.root.to_string_lossy().to_string();
    let assets_dir = layout.assets.to_string_lossy().to_string();
    let assets_index = vanilla
        .asset_index
        .as_ref()
        .map(|a| a.id.clone())
        .unwrap_or_else(|| "17".into());

    let asset_index_name = assets_index.clone();

    let mut jvm_args: Vec<String> = Vec::new();
    // Memory + common flags.
    jvm_args.push(format!("-Xmx{mem_mb}M"));
    jvm_args.push(format!("-Xms{}M", (mem_mb / 4).max(256)));
    jvm_args.push(format!("-Djava.library.path={natives_dir}"));

    // authlib-injector: for online third-party accounts (Ely.by / LittleSkin)
    // the game must talk to that server's session endpoint, not Mojang's.
    // The injector jar is fetched by the updater into `authlib-injector.jar`;
    // `authserver_url` is the Mojang-API-compatible base to point it at.
    if let Some(server) = &session.authserver_url {
        let injected = base_dir.join("authlib-injector.jar");
        if injected.is_file() {
            // One argv element; the JVM splits `-javaagent:<path>=<opts>` at
            // the first `=`, so paths with spaces (macOS Application Support)
            // survive as long as the whole flag stays a single argument.
            jvm_args.push(format!(
                "-javaagent:{}={}",
                injected.to_string_lossy(),
                server
            ));
        } else {
            // The jar is missing — skip the agent (the game would fall back
            // to Mojang sessions, i.e. only offline play works). The updater
            // normally provides it; surface a warning in the log.
            eprintln!("[alyrion] authlib-injector.jar not found — third-party online auth unavailable");
        }
    }

    // JVM args from the profiles (rule-filtered, tokens substituted later).
    let mut profile_jvm = evaluate_arguments(&vanilla.arguments.as_ref().map(|a| a.jvm.clone()).unwrap_or_default());
    profile_jvm.extend(evaluate_arguments(&neoforge.arguments.as_ref().map(|a| a.jvm.clone()).unwrap_or_default()));

    let mut game_args: Vec<String> = evaluate_arguments(&vanilla.arguments.as_ref().map(|a| a.game.clone()).unwrap_or_default());
    game_args.extend(evaluate_arguments(&neoforge.arguments.as_ref().map(|a| a.game.clone()).unwrap_or_default()));

    // Classpath token.
    let classpath_joined = cp.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>().join(sep);
    let natives_token = natives_dir.clone();
    let library_dir = layout.libraries.to_string_lossy().to_string();

    let vars: Vec<(&str, &str)> = vec![
        ("auth_player_name", &session.username),
        ("auth_uuid", &session.uuid),
        ("auth_access_token", &session.access_token),
        ("auth_session", &session.access_token),
        ("auth_xuid", "0"),
        ("clientid", "0"),
        ("user_type", &session.user_type),
        ("version_name", &profile),
        ("version_type", "release"),
        ("launcher_name", "Alyrion Launcher"),
        ("launcher_version", env!("CARGO_PKG_VERSION")),
        ("game_directory", &game_dir),
        ("game_assets", &assets_dir),
        ("assets_root", &assets_dir),
        ("assets_index_name", &asset_index_name),
        ("natives_directory", &natives_token),
        ("library_directory", &library_dir),
        ("classpath", &classpath_joined),
        ("classpath_separator", sep),
        ("resolution_width", "1280"),
        ("resolution_height", "720"),
    ];

    // Substitute tokens in profile jvm args, then prepend our own flags.
    // Every flag stays its own argv element — joining them into one string
    // breaks the JVM (it parses a single argv element as one option; verified:
    // `java "-Xmx64M -Xms32M -version"` → "Invalid maximum heap size") and
    // would mangle paths containing spaces (e.g. `~/Library/Application
    // Support/...` on macOS).
    let mut final_jvm: Vec<String> = profile_jvm
        .iter()
        .map(|a| substitute(a, &vars))
        .collect();
    let mut all_jvm = jvm_args;
    all_jvm.append(&mut final_jvm);

    // Filter out any raw -cp / -classpath from profile jvm args so we emit
    // exactly one -cp <classpath> right before the mainClass.
    let mut clean_jvm = Vec::new();
    let mut skip_next = false;
    for arg in &all_jvm {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "-cp" || arg == "-classpath" {
            skip_next = true;
            continue;
        }
        clean_jvm.push(arg.clone());
    }

    for extra in custom_jvm_args.split_whitespace() {
        if !extra.is_empty() {
            clean_jvm.push(extra.to_string());
        }
    }

    let mut final_game: Vec<String> = game_args.iter().map(|a| substitute(a, &vars)).collect();

    // NeoForge FML args are already in the merged profile game args.
    // Ensure the FML version dies are present (bootstraplauncher requires them).
    let fml_required = [
        ("--fml.mcVersion", MC_VERSION),
        ("--fml.neoForgeVersion", NEOFORGE_VERSION),
        ("--fml.fmlVersion", "4.0.42"),
        ("--fml.neoFormVersion", "20240808.144430"),
        ("--launchTarget", "forgeclient"),
    ];
    for (k, v) in fml_required {
        if !final_game.iter().any(|a| a == k) {
            final_game.push(k.to_string());
            final_game.push(v.to_string());
        }
    }

    let main_class = neoforge
        .main_class
        .clone()
        .unwrap_or_else(|| "cpw.mods.bootstraplauncher.BootstrapLauncher".into());

    let mut args: Vec<String> = Vec::new();
    args.extend(clean_jvm);
    args.push("-cp".to_string());
    args.push(classpath_joined.clone());
    args.push(main_class);
    args.extend(final_game);

    // `JAVA_HOME` must point at a real JDK root. For `/usr/bin/java` (the
    // macOS stub) `bin/..` is `/usr`, which is not a JDK home — check for the
    // `release` marker file every JDK ships at its root before using it.
    let java_home = java
        .path
        .parent()
        .and_then(|p| p.parent())
        .filter(|p| p.join("release").is_file())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    Ok(LaunchSpec {
        java: java.clone(),
        cwd: layout.root.clone(),
        args,
        envs: vec![("JAVA_HOME".to_string(), java_home)],
        log_path: layout.logs.join("latest.log"),
    })
}

/// Spawn the game, streaming output to a log file. Returns the child.
pub fn spawn_game(
    spec: &LaunchSpec,
    log_path: Option<&Path>,
) -> Result<std::process::Child, GameError> {
    let path = log_path.unwrap_or(&spec.log_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let log = fs::File::create(path)?;
    let err_log = log.try_clone()?;
    let child = Command::new(&spec.java.path)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .envs(spec.envs.iter().cloned())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(err_log))
        .spawn()?;
    Ok(child)
}

/// Wait for a child to exit with a timeout (test helper).
#[allow(dead_code)]
pub fn wait_with_timeout(child: &mut std::process::Child, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse() {
        let v: McVersionJson = serde_json::from_str(
            r#"{"id":"x","mainClass":"a.B","libraries":[],"arguments":{"game":["a"],"jvm":[]}}"#,
        )
        .unwrap();
        assert_eq!(v.main_class.as_deref(), Some("a.B"));
    }

    #[test]
    fn java_version_parse() {
        // The parser scans each line for a `version "…"` fragment.
        assert_eq!(parse_java_major("version \"21.0.12\""), 21);
        assert_eq!(parse_java_major("java version \"1.8.0_401\""), 8);
        assert_eq!(parse_java_major("openjdk version \"17.0.1\" 2025-01-21"), 17);
        assert_eq!(parse_java_major("version \"11.0.22\""), 11);
    }

    #[test]
    fn rules_filter() {
        let os = current_os_name();
        // A lib disallowed on some *other* OS always applies here.
        let other_os = if os == "windows" { "linux" } else { "windows" };
        let lib: VerLibrary = serde_json::from_str(&format!(
            r#"{{"name":"a:b:1","rules":[{{"action":"disallow","os":{{"name":"{other_os}"}}}}]}}"#
        ))
        .unwrap();
        assert!(lib.applies_to_current_os());
        // A lib forbidden on the current OS does not apply.
        let lib2: VerLibrary = serde_json::from_str(&format!(
            r#"{{"name":"a:b:1","rules":[{{"action":"disallow","os":{{"name":"{os}"}}}}]}}"#
        ))
        .unwrap();
        assert!(!lib2.applies_to_current_os());
    }

    #[test]
    fn substitution() {
        let vars = vec![
            ("auth_player_name", "Tester"),
            ("library_directory", "/custom/libs"),
        ];
        assert_eq!(
            substitute("--username ${auth_player_name}", &vars),
            "--username Tester"
        );
        assert_eq!(
            substitute("-DlibraryDirectory=${library_directory}", &vars),
            "-DlibraryDirectory=/custom/libs"
        );
    }

    #[test]
    fn evaluate_args_rules() {
        let os = current_os_name();
        let other_os = if os == "osx" { "windows" } else { "osx" };
        let json: Vec<serde_json::Value> = serde_json::from_str(&format!(
            r#"[
                "-Dsimple=1",
                {{"rules": [{{"action": "allow", "os": {{"name": "{os}"}}}}], "value": ["-Xmatching"]}},
                {{"rules": [{{"action": "allow", "os": {{"name": "{other_os}"}}}}], "value": ["-Xother"]}}
            ]"#
        )).unwrap();

        let evaluated = evaluate_arguments(&json);
        assert_eq!(evaluated, vec!["-Dsimple=1", "-Xmatching"]);
    }
}