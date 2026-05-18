//! `SeaORM`-backed implementation of [`TenantRepo`].
//!
//! Implementation is split across siblings (`reads`, `lifecycle`,
//! `updates`, `retention`, `integrity`, `helpers`) — each method on the
//! [`TenantRepo`] trait dispatches to a `pub(super)` free function in
//! the matching submodule.

pub mod conversion;
mod helpers;
mod integrity;
mod lifecycle;
pub mod metadata;
mod reads;
mod retention;
mod updates;

pub use conversion::ConversionRepoImpl;
pub use metadata::MetadataRepoImpl;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use modkit_db::DBProvider;
use modkit_security::AccessScope;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use account_management_sdk::UpdateTenantRequest;
use modkit_odata::{ODataQuery, Page};

use crate::domain::error::DomainError;
use crate::domain::tenant::closure::ClosureRow;
use crate::domain::tenant::integrity::{IntegrityCategory, Violation};
use crate::domain::tenant::model::{ChildCountFilter, NewTenant, TenantModel, TenantStatus};
use crate::domain::tenant::repo::TenantRepo;
use crate::domain::tenant::retention::{
    HardDeleteEligibility, HardDeleteOutcome, TenantProvisioningRow, TenantRetentionRow,
};

/// Shared alias used by tests.
pub type AmDbProvider = DBProvider<DomainError>;

/// `SeaORM` repository adapter for [`TenantRepo`].
pub struct TenantRepoImpl {
    db: Arc<AmDbProvider>,
}

impl TenantRepoImpl {
    #[must_use]
    pub fn new(db: Arc<AmDbProvider>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl TenantRepo for TenantRepoImpl {
    async fn find_by_id(
        &self,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<TenantModel>, DomainError> {
        reads::find_by_id(self, scope, id).await
    }

    async fn find_many(
        &self,
        scope: &AccessScope,
        ids: &[Uuid],
    ) -> Result<Vec<TenantModel>, DomainError> {
        reads::find_many(self, scope, ids).await
    }

    async fn list_children(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<TenantModel>, DomainError> {
        reads::list_children(self, scope, parent_id, query).await
    }

    async fn insert_provisioning(
        &self,
        scope: &AccessScope,
        tenant: &NewTenant,
    ) -> Result<TenantModel, DomainError> {
        lifecycle::insert_provisioning(self, scope, tenant).await
    }

    async fn activate_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        closure_rows: &[ClosureRow],
        idp_metadata: Option<&Value>,
    ) -> Result<TenantModel, DomainError> {
        lifecycle::activate_tenant(self, scope, tenant_id, closure_rows, idp_metadata).await
    }

    async fn find_idp_metadata(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<Value>, DomainError> {
        reads::find_idp_metadata(self, scope, tenant_id).await
    }

    async fn upsert_idp_metadata(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        idp_metadata: Option<&Value>,
    ) -> Result<(), DomainError> {
        lifecycle::upsert_idp_metadata(self, scope, tenant_id, idp_metadata).await
    }

    async fn compensate_provisioning(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        expected_claimed_by: Option<Uuid>,
    ) -> Result<(), DomainError> {
        lifecycle::compensate_provisioning(self, scope, tenant_id, expected_claimed_by).await
    }

    async fn update_tenant_mutable(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        patch: &UpdateTenantRequest,
    ) -> Result<TenantModel, DomainError> {
        updates::update_tenant_mutable(self, scope, tenant_id, patch).await
    }

    async fn set_status(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        new_status: TenantStatus,
        now: OffsetDateTime,
    ) -> Result<TenantModel, DomainError> {
        updates::set_status(self, scope, tenant_id, new_status, now).await
    }

    async fn load_ancestor_chain_through_parent(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
    ) -> Result<Vec<TenantModel>, DomainError> {
        updates::load_ancestor_chain_through_parent(self, scope, parent_id).await
    }

    async fn scan_retention_due(
        &self,
        scope: &AccessScope,
        now: OffsetDateTime,
        default_retention: Duration,
        limit: usize,
    ) -> Result<Vec<TenantRetentionRow>, DomainError> {
        retention::scan_retention_due(self, scope, now, default_retention, limit).await
    }

    async fn clear_retention_claim(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        worker_id: Uuid,
    ) -> Result<(), DomainError> {
        retention::clear_retention_claim(self, scope, tenant_id, worker_id).await
    }

    async fn scan_stuck_provisioning(
        &self,
        scope: &AccessScope,
        now: OffsetDateTime,
        older_than: OffsetDateTime,
        limit: usize,
    ) -> Result<Vec<TenantProvisioningRow>, DomainError> {
        retention::scan_stuck_provisioning(self, scope, now, older_than, limit).await
    }

    async fn count_children(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
        filter: ChildCountFilter,
    ) -> Result<u64, DomainError> {
        reads::count_children(self, scope, parent_id, filter).await
    }

    async fn schedule_deletion(
        &self,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
        retention: Option<Duration>,
    ) -> Result<TenantModel, DomainError> {
        updates::schedule_deletion(self, scope, id, now, retention).await
    }

    async fn check_hard_delete_eligibility(
        &self,
        scope: &AccessScope,
        id: Uuid,
        claimed_by: Uuid,
    ) -> Result<HardDeleteEligibility, DomainError> {
        lifecycle::check_hard_delete_eligibility(self, scope, id, claimed_by).await
    }

    async fn hard_delete_one(
        &self,
        scope: &AccessScope,
        id: Uuid,
        claimed_by: Uuid,
    ) -> Result<HardDeleteOutcome, DomainError> {
        lifecycle::hard_delete_one(self, scope, id, claimed_by).await
    }

    async fn mark_provisioning_terminal_failure(
        &self,
        scope: &AccessScope,
        id: Uuid,
        claimed_by: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        lifecycle::mark_provisioning_terminal_failure(self, scope, id, claimed_by, now).await
    }

    async fn mark_retention_terminal_failure(
        &self,
        scope: &AccessScope,
        id: Uuid,
        claimed_by: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        lifecycle::mark_retention_terminal_failure(self, scope, id, claimed_by, now).await
    }

    async fn is_descendant(
        &self,
        scope: &AccessScope,
        ancestor: Uuid,
        descendant: Uuid,
    ) -> Result<bool, DomainError> {
        reads::is_descendant(self, scope, ancestor, descendant).await
    }

    async fn run_integrity_check(
        &self,
        scope: &AccessScope,
    ) -> Result<Vec<(IntegrityCategory, Violation)>, DomainError> {
        integrity::run_integrity_check(self, scope).await
    }

    async fn repair_derivable_closure_violations(
        &self,
        scope: &AccessScope,
    ) -> Result<crate::domain::tenant::integrity::RepairReport, DomainError> {
        integrity::repair_derivable_closure_violations(self, scope).await
    }
}
