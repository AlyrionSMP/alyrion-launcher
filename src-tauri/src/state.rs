//! Launcher state machine.
//!
//! The UI is a pure projection of this state. Play is only ever allowed in
//! `Ready`; any other phase (including all update phases) blocks it, which
//! enforces "you cannot play while an update is ongoing".

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// No pack installed yet; first-run setup in progress.
    #[default]
    Boot,
    /// Checking remote metadata.
    Checking,
    /// Downloading / verifying the pack.
    Downloading,
    /// Installing (extracting, finalizing).
    Installing,
    /// Fully up to date and ready to play.
    Ready,
    /// Game is launching / running.
    Launching,
    /// Game process running.
    Running,
    /// An error occurred; message surfaced to UI.
    Error,
}

/// A snapshot of the whole launcher state, sent to the UI on every change.
#[derive(Debug, Clone, Serialize)]
pub struct UiState {
    pub phase: Phase,
    pub progress: Option<StageProgress>,
    pub installed_version: Option<InstalledInfo>,
    pub latest_version: Option<String>,
    pub session: Option<SessionInfo>,
    pub java: Option<JavaInfoUi>,
    pub game_running: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageProgress {
    pub stage: String,
    pub fraction: f32,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledInfo {
    pub version_number: String,
    pub version_id: String,
    pub mods: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub username: String,
    pub uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JavaInfoUi {
    pub major: u16,
    pub path: String,
}

#[derive(Debug, Clone, Default)]
pub struct Inner {
    pub phase: Phase,
    pub progress: Option<StageProgress>,
    pub installed_version: Option<InstalledInfo>,
    pub latest_version: Option<String>,
    pub session: Option<SessionInfo>,
    pub java: Option<JavaInfoUi>,
    pub game_running: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct SharedState {
    pub inner: Arc<Mutex<Inner>>,
    pub dirty: Arc<AtomicBool>,
}

impl SharedState {
    pub fn new() -> Self {
        SharedState {
            inner: Arc::new(Mutex::new(Inner::default())),
            dirty: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn snapshot(&self) -> UiState {
        let g = self.inner.lock().unwrap();
        UiState {
            phase: g.phase,
            progress: g.progress.clone(),
            installed_version: g.installed_version.clone(),
            latest_version: g.latest_version.clone(),
            session: g.session.clone(),
            java: g.java.clone(),
            game_running: g.game_running,
            error: g.error.clone(),
        }
    }

    pub fn set_phase(&self, phase: Phase) {
        self.inner.lock().unwrap().phase = phase;
        self.dirty.store(true, Ordering::Release);
    }

    pub fn poll_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn set_progress(&self, stage: &str, fraction: f32, detail: &str) {
        let mut g = self.inner.lock().unwrap();
        g.progress = Some(StageProgress {
            stage: stage.into(),
            fraction,
            detail: detail.into(),
        });
        self.dirty.store(true, Ordering::Release);
    }

    pub fn set_installed(&self, info: Option<InstalledInfo>) {
        self.inner.lock().unwrap().installed_version = info;
        self.dirty.store(true, Ordering::Release);
    }

    pub fn set_latest(&self, version: Option<String>) {
        self.inner.lock().unwrap().latest_version = version;
        self.dirty.store(true, Ordering::Release);
    }

    pub fn set_session(&self, session: Option<SessionInfo>) {
        self.inner.lock().unwrap().session = session;
        self.dirty.store(true, Ordering::Release);
    }

    pub fn set_java(&self, java: Option<JavaInfoUi>) {
        self.inner.lock().unwrap().java = java;
        self.dirty.store(true, Ordering::Release);
    }

    pub fn set_game_running(&self, running: bool) {
        self.inner.lock().unwrap().game_running = running;
        self.dirty.store(true, Ordering::Release);
    }

    pub fn set_error(&self, error: Option<String>) {
        self.inner.lock().unwrap().error = error;
        self.dirty.store(true, Ordering::Release);
    }

    /// Play is only allowed when installed, no update in flight and no error.
    pub fn can_play(&self) -> bool {
        let g = self.inner.lock().unwrap();
        g.phase == Phase::Ready
            && g.installed_version.is_some()
            && g.error.is_none()
            && !g.game_running
    }
}