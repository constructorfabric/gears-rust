//! In-memory [`PendingDeprovisionRepo`] for service-level tests.

use std::collections::HashSet;
use std::sync::Mutex;

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::user::pending_deprovision::PendingDeprovisionRepo;

/// Holds the recorded `(tenant_id, user_id)` pairs in a set.
#[derive(Default)]
pub struct FakePendingDeprovisions {
    recorded: Mutex<HashSet<(Uuid, Uuid)>>,
}

impl FakePendingDeprovisions {
    /// Whether the deletion of `user_id` from `tenant_id` is recorded.
    pub fn contains(&self, tenant_id: Uuid, user_id: Uuid) -> bool {
        self.recorded
            .lock()
            .expect("lock")
            .contains(&(tenant_id, user_id))
    }
}

#[async_trait]
impl PendingDeprovisionRepo for FakePendingDeprovisions {
    async fn record(&self, tenant_id: Uuid, user_id: Uuid) -> Result<(), DomainError> {
        self.recorded
            .lock()
            .expect("lock")
            .insert((tenant_id, user_id));
        Ok(())
    }

    async fn is_recorded(&self, tenant_id: Uuid, user_id: Uuid) -> Result<bool, DomainError> {
        Ok(self.contains(tenant_id, user_id))
    }

    async fn clear(&self, tenant_id: Uuid, user_id: Uuid) -> Result<(), DomainError> {
        self.recorded
            .lock()
            .expect("lock")
            .remove(&(tenant_id, user_id));
        Ok(())
    }
}
