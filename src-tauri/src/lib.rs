//! Alyrion Launcher — Tauri v2 application.
//!
//! Single binary with a small state machine. The UI polls `state_snapshot`
//! and the Rust side emits `state-changed` events on every transition.
//! Play is only possible from the `Ready` phase — never while an update is
//! in flight.

#![allow(dead_code, unused_imports)]

mod accounts;
mod cancellation;
mod game;
mod install;
mod java;
mod jobs;
mod maven;
mod modrinth;
mod state;
mod update;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

use state::{Phase, SharedState};

/// Resolve the per-user data root: `<app-config>/AlyrionLauncher/`.
fn resolve_data_dir(app: &tauri::App) -> PathBuf {
    let dir = app
        .path()
        .app_config_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("alyrion-launcher"));
    let dir = dir.join("AlyrionLauncher");
    std::fs::create_dir_all(&dir).ok();
    dir
}

#[derive(Clone)]
struct Ctx {
    shared: SharedState,
    base_dir: PathBuf,
    client: reqwest::Client,
    update_lock: Arc<Mutex<bool>>,
    cancel: cancellation::CancelToken,
    accounts: Arc<Mutex<Vec<accounts::Account>>>,
    game_child: Arc<Mutex<Option<std::process::Child>>>,
}

impl Ctx {
    fn new(base_dir: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .user_agent("Alyrion-Launcher/1.0 (alyrion launcher)")
            .build()
            .expect("reqwest client");
        let accounts = accounts::load_accounts(&base_dir);
        let shared = SharedState::new();
        if let Ok(j) = java::find_java(&base_dir) {
            shared.set_java(Some(state::JavaInfoUi {
                major: j.major,
                path: j.path.to_string_lossy().to_string(),
            }));
        }
        Ctx {
            shared,
            base_dir,
            client,
            update_lock: Arc::new(Mutex::new(false)),
            cancel: cancellation::CancelToken::new(),
            accounts: Arc::new(Mutex::new(accounts)),
            game_child: Arc::new(Mutex::new(None)),
        }
    }
}

/// Broadcast current state to all connected frontend listeners.
fn emit_state(app: &tauri::AppHandle) {
    if let Some(ctx) = app.try_state::<Ctx>() {
        let _ = app.emit("state-changed", ctx.shared.snapshot());
    }
}

#[tauri::command]
fn state_snapshot(app: tauri::AppHandle) -> state::UiState {
    app.state::<Ctx>().shared.snapshot()
}

/// Start (or resume) checking for updates and installing the latest version.
/// Runs on a background task; refuses to run while a game is up.
#[tauri::command]
async fn start_update(app: tauri::AppHandle) -> Result<(), String> {
    let ctx = app.state::<Ctx>();
    let mut guard = ctx.update_lock.lock().unwrap();
    if *guard {
        return Ok(()); // already updating
    }
    *guard = true;
    drop(guard);

    let base = ctx.base_dir.clone();
    let client = ctx.client.clone();
    let shared = ctx.shared.clone();
    let cancel = ctx.cancel.clone();
    cancel.reset();
    let app_for_lock = app.clone();

    shared.set_phase(Phase::Checking);
    shared.set_error(None);

    tauri::async_runtime::spawn(async move {
        let outcome = update::update_pack(&client, &base, cancel.as_atomic(), |p| {
            shared.set_progress(p.stage.as_str(), p.fraction, &p.detail);
        })
        .await;
        match outcome {
            Ok(outcome) => {
                if let Ok(j) = java::find_java(&base) {
                    shared.set_java(Some(state::JavaInfoUi {
                        major: j.major,
                        path: j.path.to_string_lossy().to_string(),
                    }));
                }
                shared.set_installed(Some(state::InstalledInfo {
                    version_number: outcome.version().to_string(),
                    version_id: outcome.version_id().to_string(),
                    mods: 0,
                }));
                shared.set_latest(Some(outcome.version().to_string()));
                shared.set_error(None);
                shared.set_progress("done", 1.0, "Ready to play");
                shared.set_phase(Phase::Ready);
            }
            Err(e) => {
                shared.set_error(Some(e.to_string()));
                shared.set_phase(Phase::Error);
            }
        }
        {
            let ctx = app_for_lock.state::<Ctx>();
            *ctx.update_lock.lock().unwrap() = false;
        }
        emit_state(&app_for_lock);
    });
    Ok(())
}

#[tauri::command]
fn cancel_update(app: tauri::AppHandle) -> Result<(), String> {
    let ctx = app.state::<Ctx>();
    ctx.cancel.cancel();
    Ok(())
}

/// Latest pack version metadata + changelog (Markdown) for the news panel.
#[tauri::command]
async fn latest_changelog(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let ctx = app.state::<Ctx>();
    let v = modrinth::fetch_latest_version(&ctx.client)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "version_number": v.version_number,
        "version_type": v.version_type,
        "date_published": v.date_published,
        "changelog": v.changelog.unwrap_or_default(),
    }))
}

/// List saved accounts (username + provider only — never tokens).
#[tauri::command]
fn list_accounts(app: tauri::AppHandle) -> Vec<serde_json::Value> {
    let ctx = app.state::<Ctx>();
    let accs = ctx.accounts.lock().unwrap();
    accs.iter()
        .map(|a| {
            serde_json::json!({
                "username": a.username,
                "provider": a.provider.as_str(),
                "uuid": a.uuid,
            })
        })
        .collect()
}

/// Log in as an offline (cracked) player.
#[tauri::command]
fn login_offline(app: tauri::AppHandle, username: String) -> Result<(), String> {
    let ctx = app.state::<Ctx>();
    let acc = accounts::make_offline(&username);
    let mut accs = ctx.accounts.lock().unwrap();
    // Replace any existing offline account for the same name.
    accs.retain(|a| !(a.provider == accounts::Provider::Offline && a.username == acc.username));
    accs.push(acc.clone());
    accounts::save_accounts(&ctx.base_dir, &accs).map_err(|e| e.to_string())?;
    ctx.shared.set_session(Some(state::SessionInfo {
        username: acc.username.clone(),
        uuid: acc.uuid.clone(),
        provider: Some(acc.provider.as_str().to_string()),
    }));
    ctx.shared.poll_dirty();
    let _ = emit_state(&app);
    Ok(())
}

/// Log in with LittleSkin (Yggdrasil). Credentials are sent over HTTPS to the
/// chosen server only; tokens are stored, never the password.
#[tauri::command]
async fn login_littleskin(
    app: tauri::AppHandle,
    username: String,
    password: String,
) -> Result<(), String> {
    let ctx = app.state::<Ctx>();
    let settings = accounts::load_settings(&ctx.base_dir);
    let server = settings.littleskin_server.clone();
    let acc = accounts::littleskin_authenticate(&ctx.client, &server, &username, &password)
        .await
        .map_err(|e| e.to_string())?;
    let mut accs = ctx.accounts.lock().unwrap();
    accs.retain(|a| a.provider != accounts::Provider::LittleSkin);
    accs.push(acc.clone());
    accounts::save_accounts(&ctx.base_dir, &accs).map_err(|e| e.to_string())?;
    ctx.shared.set_session(Some(state::SessionInfo {
        username: acc.username.clone(),
        uuid: acc.uuid.clone(),
        provider: Some(acc.provider.as_str().to_string()),
    }));
    ctx.shared.poll_dirty();
    let _ = emit_state(&app);
    Ok(())
}

/// Log in with Ely.by using direct credentials (Yggdrasil authserver —
/// no OAuth app, same as XMCL). Password is sent once over HTTPS and never
/// stored; only the token is kept.
#[tauri::command]
async fn login_elyby(
    app: tauri::AppHandle,
    username: String,
    password: String,
) -> Result<(), String> {
    let ctx = app.state::<Ctx>();
    let acc = accounts::elyby_authenticate(&ctx.client, &username, &password)
        .await
        .map_err(|e| e.to_string())?;
    let mut accs = ctx.accounts.lock().unwrap();
    accs.retain(|a| a.provider != accounts::Provider::ElyBy);
    accs.push(acc.clone());
    accounts::save_accounts(&ctx.base_dir, &accs).map_err(|e| e.to_string())?;
    ctx.shared.set_session(Some(state::SessionInfo {
        username: acc.username.clone(),
        uuid: acc.uuid.clone(),
        provider: Some(acc.provider.as_str().to_string()),
    }));
    ctx.shared.poll_dirty();
    let _ = emit_state(&app);
    Ok(())
}

/// Remove all saved accounts.
#[tauri::command]
fn logout(app: tauri::AppHandle) -> Result<(), String> {
    let ctx = app.state::<Ctx>();
    ctx.accounts.lock().unwrap().clear();
    accounts::save_accounts(&ctx.base_dir, &[]).map_err(|e| e.to_string())?;
    ctx.shared.set_session(None);
    ctx.shared.poll_dirty();
    let _ = emit_state(&app);
    Ok(())
}

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> Result<accounts::Settings, String> {
    let ctx = app.state::<Ctx>();
    Ok(accounts::load_settings(&ctx.base_dir))
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, settings: accounts::Settings) -> Result<(), String> {
    let ctx = app.state::<Ctx>();
    accounts::save_settings(&ctx.base_dir, &settings).map_err(|e| e.to_string())?;
    Ok(())
}

/// Launch the game. Only valid in `Ready`, only with a selected account.
#[tauri::command]
async fn play(app: tauri::AppHandle) -> Result<(), String> {
    let ctx = app.state::<Ctx>();
    let shared = ctx.shared.clone();
    if !shared.can_play() {
        return Err("An update is in progress — play is disabled".into());
    }
    let session = shared
        .inner
        .lock()
        .unwrap()
        .session
        .clone()
        .ok_or_else(|| "log in first (Offline, LittleSkin or Ely.by)".to_string())?;

    // Resolve everything fallible *before* mutating state, so a failure
    // (missing Java, broken instance, …) can never leave the UI stuck on
    // "Preparing launch…" with play permanently disabled.
    let java = game::find_java(&ctx.base_dir).map_err(|e| e.to_string())?;

    // Look up the account to get the full access token.
    let acc = {
        let accs = ctx.accounts.lock().unwrap();
        accs.iter()
            .find(|a| a.username == session.username)
            .cloned()
    };
    let acc = acc.unwrap_or_else(|| accounts::make_offline(&session.username));

    shared.set_phase(Phase::Launching);
    shared.set_game_running(true);
    shared.set_java(Some(state::JavaInfoUi {
        major: java.major,
        path: java.path.to_string_lossy().to_string(),
    }));

    // Ensure all libraries and natives are present on disk before launching.
    let layout = game::InstanceLayout::new(&ctx.base_dir);
    if let (Ok(vanilla), Some(profile)) = (
        game::read_version_json(&layout, game::MC_VERSION),
        game::find_installed_neoforge_profile(&layout),
    ) {
        if let Ok(neoforge) = game::read_version_json(&layout, &profile) {
            let merged = game::merged_libraries(&vanilla, &neoforge);
            let dummy_cancel = std::sync::atomic::AtomicBool::new(false);
            let _ = game::sync_libraries(&ctx.client, &layout, &merged, &dummy_cancel, |_, _| {}).await;
            let _ = game::extract_natives(&layout, &merged);
        }
    }

    let settings = accounts::load_settings(&ctx.base_dir);
    let spec = game::build_launch_spec(
        &ctx.base_dir,
        &java,
        &game::Session {
            username: acc.username.clone(),
            uuid: acc.uuid_dashless(),
            access_token: acc.access_token.clone(),
            user_type: acc.user_type().into(),
            // Point authlib-injector at the right session server for
            // third-party online accounts.
            authserver_url: match acc.provider {
                accounts::Provider::ElyBy => {
                    Some(accounts::ELYBY_AUTHSERVER.to_string())
                }
                accounts::Provider::LittleSkin => {
                    Some(settings.littleskin_server.clone())
                }
                accounts::Provider::Offline => None,
            },
        },
        settings.allocated_memory_mb,
        &settings.jvm_args,
    )
    .map_err(|e| {
        shared.set_game_running(false);
        shared.set_phase(Phase::Ready);
        e.to_string()
    })?;

    match game::spawn_game(&spec, None) {
        Ok(child) => {
            shared.set_phase(Phase::Running);
            *ctx.game_child.lock().unwrap() = Some(child);
            jobs::spawn_proc_watcher(ctx.game_child.clone(), move || {
                shared.set_game_running(false);
                if shared.inner.lock().unwrap().phase == Phase::Running {
                    shared.set_phase(Phase::Ready);
                }
            });
        }
        Err(e) => {
            shared.set_game_running(false);
            shared.set_error(Some(e.to_string()));
            shared.set_phase(Phase::Error);
            return Err(e.to_string());
        }
    }
    Ok(())
}

/// Terminate the running game process if active.
#[tauri::command]
fn kill_game(app: tauri::AppHandle) -> Result<(), String> {
    let ctx = app.state::<Ctx>();
    let mut lock = ctx.game_child.lock().unwrap();
    if let Some(child) = lock.as_mut() {
        let _ = child.kill();
    }
    Ok(())
}

/// Kick off the auto-update check shortly after the window is ready.
fn auto_begin(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
        let _ = start_update(app).await;
    });
}

/// One-shot state broadcast loop (drives UI without the frontend polling
/// constantly; also covers changes not caused by commands).
fn state_loop(app: &tauri::AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(150));
        loop {
            ticker.tick().await;
            if let Some(ctx) = handle.try_state::<Ctx>() {
                if ctx.shared.poll_dirty() {
                    let _ = handle.emit("state-changed", ctx.shared.snapshot());
                }
            }
        }
    });
}

/// On startup, restore the saved account: refresh its token if the provider
/// supports it, and set it as the active session.
async fn restore_session(app: tauri::AppHandle) {
    let ctx = app.state::<Ctx>();
    let settings = accounts::load_settings(&ctx.base_dir);
    let saved = {
        let accs = ctx.accounts.lock().unwrap();
        accs.first().cloned()
    };
    let Some(mut acc) = saved else {
        return;
    };
    // Try refresh (non-fatal on failure; offline accounts stay as-is).
    let refreshed = match acc.provider {
        accounts::Provider::ElyBy => {
            accounts::elyby_refresh(&ctx.client, &acc).await
        }
        accounts::Provider::LittleSkin => {
            accounts::littleskin_refresh(&ctx.client, &settings.littleskin_server, &acc).await
        }
        accounts::Provider::Offline => Ok(acc.clone()),
    };
    if let Ok(new_acc) = refreshed {
        acc = new_acc;
        let mut accs = ctx.accounts.lock().unwrap();
        accs.retain(|a| a.provider != acc.provider);
        accs.insert(0, acc.clone());
        let _ = accounts::save_accounts(&ctx.base_dir, &accs);
    }
    ctx.shared.set_session(Some(state::SessionInfo {
        username: acc.username.clone(),
        uuid: acc.uuid.clone(),
        provider: Some(acc.provider.as_str().to_string()),
    }));
    ctx.shared.poll_dirty();
    let _ = emit_state(&app);
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            state_snapshot,
            start_update,
            cancel_update,
            latest_changelog,
            play,
            kill_game,
            list_accounts,
            login_offline,
            login_littleskin,
            login_elyby,
            logout,
            get_settings,
            save_settings
        ])
        .setup(|app| {
            app.manage(Ctx::new(resolve_data_dir(app)));
            let handle = app.handle().clone();
            state_loop(&handle);
            // Restore the session before auto-update finishes (it's fast).
            tauri::async_runtime::spawn(restore_session(handle.clone()));
            auto_begin(&handle);
            Ok(())
        });
    let app = builder
        .build(tauri::generate_context!())
        .expect("error while building tauri application");
    app.run(|_handle, _event| {});
}