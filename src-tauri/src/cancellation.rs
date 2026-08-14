//! Cancellation token shared between update jobs and UI requesters.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        CancelToken(Arc::new(AtomicBool::new(false)))
    }
    pub fn as_atomic(&self) -> &AtomicBool {
        &self.0
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn reset(&self) {
        self.0.store(false, Ordering::Release);
    }
    pub fn is_canceled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}