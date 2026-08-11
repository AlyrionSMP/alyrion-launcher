//! Background job supervisor: runs long-lived async tasks (update, download,
//! game process) with a handle that can cancel / report status, plus a
//! dedicated watcher thread for the spawned game process.

use crate::cancellation::CancelToken;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A named job with cancellation + status bits.
pub struct Job {
    pub name: &'static str,
    pub cancel: CancelToken,
    pub finished: Arc<AtomicBool>,
    pub result: Arc<Mutex<Option<String>>>,
}

impl Job {
    pub fn new(name: &'static str) -> Self {
        Job {
            name,
            cancel: CancelToken::new(),
            finished: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
        }
    }

    pub fn finish(&self, result: String) {
        *self.result.lock().unwrap() = Some(result);
        self.finished.store(true, Ordering::Release);
    }

    pub fn fail(&self, err: impl Into<String>) {
        self.finish(format!("ERR: {}", err.into()));
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
        self.finish("canceled".into());
    }
}

/// Registry of active jobs, keyed by name (only one update at a time).
pub struct Jobs {
    pub update: Option<Arc<Job>>,
}

impl Jobs {
    pub fn new() -> Self {
        Jobs { update: None }
    }
}

/// Spawn a "fence" thread that waits for a child process and flips a flag.
pub fn spawn_proc_watcher(
    mut child: std::process::Child,
    on_finish: impl Fn() + Send + 'static,
) {
    std::thread::spawn(move || {
        let _ = child.wait();
        // Give a grace period so any final log flush lands.
        std::thread::sleep(Duration::from_millis(100));
        on_finish();
    });
}

/// Poll a child process handle for exit (for the run loop that needs to
/// observe while also servicing commands).
pub fn try_wait(child: &mut std::process::Child) -> Option<i32> {
    child.try_wait().ok().flatten().map(|s| s.code().unwrap_or(-1))
}