//! A point in production code where a test can hold one request, the way
//! types-registry's `PausePoint` does: arm it, wait until a request reaches it,
//! do whatever must happen in that window, release it. Disarmed (the default)
//! it lets every request straight through.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// Cloneable; every clone is the same point.
#[derive(Clone, Default)]
pub struct PausePoint {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    armed: AtomicBool,
    reached: Notify,
    released: Notify,
}

impl PausePoint {
    /// Holds the next request that reaches this point, and only that one.
    pub fn arm(&self) {
        self.inner.armed.store(true, Ordering::SeqCst);
    }

    /// Waits until the armed request is held here.
    pub async fn reached(&self) {
        self.inner.reached.notified().await;
    }

    /// Lets the held request continue.
    pub fn release(&self) {
        self.inner.released.notify_one();
    }

    /// Called by the production code at the point: returns at once unless
    /// armed, in which case it disarms, reports the arrival and waits for
    /// [`Self::release`].
    pub(crate) async fn pass(&self) {
        if self.inner.armed.swap(false, Ordering::SeqCst) {
            self.inner.reached.notify_one();
            self.inner.released.notified().await;
        }
    }
}
