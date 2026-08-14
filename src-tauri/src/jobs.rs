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

/// Spawn a watcher thread that periodically checks the child process exit and invokes on_finish.
pub fn spawn_proc_watcher(
    child_arc: Arc<Mutex<Option<std::process::Child>>>,
    on_finish: impl Fn() + Send + 'static,
) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(150));
            let mut lock = child_arc.lock().unwrap();
            if let Some(child) = lock.as_mut() {
                match child.try_wait() {
                    Ok(Some(_status)) => {
                        *lock = None;
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        *lock = None;
                        break;
                    }
                }
            } else {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        on_finish();
    });
}