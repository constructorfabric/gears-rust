//! The notification delivery primitive of the storage contract, forwarded to
//! the [`NotificationPipeline`] port.

use std::sync::Arc;

use quota_enforcement_sdk::{NotificationDeliveryHandle, NotificationDeliveryV1, StorageError};

use super::bootstrap::StoragePlugin;
use super::ports::NotificationPipeline;

impl StoragePlugin {
    /// Start the plugin's one notification pipeline, handing every claimed
    /// event to `delivery`.
    ///
    /// # Errors
    ///
    /// `Internal` when the plugin has no pipeline or already started it;
    /// `Unavailable` when the backend cannot start it.
    pub async fn start_notification_delivery(
        &self,
        delivery: Arc<dyn NotificationDeliveryV1>,
    ) -> Result<Box<dyn NotificationDeliveryHandle>, StorageError> {
        let pipeline: &Arc<dyn NotificationPipeline> =
            self.notifications.as_ref().ok_or_else(|| {
                StorageError::Internal("the storage plugin has no notification pipeline".to_owned())
            })?;
        pipeline.start(delivery).await
    }
}
