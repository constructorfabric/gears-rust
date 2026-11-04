//! The running notification pipeline, held until shutdown.

use std::sync::Mutex;

use quota_enforcement_sdk::NotificationDeliveryHandle;
use toolkit_macros::domain_model;

/// Holds the handle of the pipeline bootstrap started, so the lifecycle entry
/// can stop it on shutdown. Empty until delivery starts and after it stops.
#[domain_model]
#[derive(Default)]
pub struct DeliveryLifecycle {
    handle: Mutex<Option<Box<dyn NotificationDeliveryHandle>>>,
}

impl DeliveryLifecycle {
    /// Keep `handle`. A handle already held is returned, never dropped: the
    /// caller stops it.
    pub fn hold(
        &self,
        handle: Box<dyn NotificationDeliveryHandle>,
    ) -> Option<Box<dyn NotificationDeliveryHandle>> {
        self.lock().replace(handle)
    }

    /// Whether a pipeline is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.lock().is_some()
    }

    /// Stop the pipeline, if one runs, and wait for its workers to finish.
    pub async fn stop(&self) {
        let handle = self.lock().take();
        if let Some(handle) = handle {
            handle.stop().await;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Box<dyn NotificationDeliveryHandle>>> {
        // The guarded value is replaced whole, so a poisoned lock holds a
        // consistent value.
        self.handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
