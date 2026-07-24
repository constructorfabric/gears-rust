//! `SeaORM`-backed implementation of [`SecretRepo`].

pub mod helpers;
mod reads;
mod writes;

#[cfg(test)]
mod repo_tests;

use async_trait::async_trait;
use credstore_sdk::{OwnerId, SecretRef, SharingMode, TenantId};
use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

pub use helpers::{CredstoreDbProvider, SecretRepoImpl};

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::SecretCounts;
use crate::domain::secret::model::{SecretRow, SecretStatus};
use crate::domain::secret::repo::SecretRepo;

#[async_trait]
impl SecretRepo for SecretRepoImpl {
    async fn resolve_for_get(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
    ) -> Result<Option<SecretRow>, DomainError> {
        reads::resolve_for_get(self, req_tenant, subject, key, chain).await
    }

    async fn insert_provisioning(
        &self,
        scope: &AccessScope,
        new: &crate::domain::secret::model::NewSecret,
    ) -> Result<(), DomainError> {
        writes::insert_provisioning(self, scope, new).await
    }

    async fn mark_active(&self, scope: &AccessScope, id: Uuid) -> Result<(), DomainError> {
        writes::mark_active(self, scope, id).await
    }

    async fn touch(
        &self,
        scope: &AccessScope,
        id: Uuid,
        sharing: SharingMode,
        expected_version: Option<i64>,
        expires_at: Option<OffsetDateTime>,
        value_fp: Vec<u8>,
    ) -> Result<Option<SecretRow>, DomainError> {
        writes::touch(
            self,
            scope,
            id,
            sharing,
            expected_version,
            expires_at,
            value_fp,
        )
        .await
    }

    async fn backfill_fp(
        &self,
        id: Uuid,
        value_fp: Vec<u8>,
        fp_key_id: i16,
    ) -> Result<bool, DomainError> {
        writes::backfill_fp(self, id, value_fp, fp_key_id).await
    }

    async fn list_unfenced(&self, limit: u64) -> Result<Vec<SecretRow>, DomainError> {
        writes::list_unfenced(self, limit).await
    }

    async fn find_own(
        &self,
        scope: &AccessScope,
        tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
    ) -> Result<Option<SecretRow>, DomainError> {
        reads::find_own(self, scope, tenant, subject, key).await
    }

    async fn find_for_write(
        &self,
        scope: &AccessScope,
        tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        sharing: SharingMode,
    ) -> Result<Option<SecretRow>, DomainError> {
        reads::find_for_write(self, scope, tenant, subject, key, sharing).await
    }

    async fn delete_by_id(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<(), DomainError> {
        writes::delete_by_id(self, scope, id, expected_version).await
    }

    async fn mark_deprovisioning(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<bool, DomainError> {
        writes::mark_deprovisioning(self, scope, id, expected_version).await
    }

    async fn list_stale_pending(
        &self,
        provisioning_older_than_secs: u64,
        deprovisioning_older_than_secs: u64,
        limit: u64,
    ) -> Result<Vec<SecretRow>, DomainError> {
        writes::list_stale_pending(
            self,
            provisioning_older_than_secs,
            deprovisioning_older_than_secs,
            limit,
        )
        .await
    }

    async fn reap_by_id(&self, id: Uuid, expected: SecretStatus) -> Result<bool, DomainError> {
        writes::reap_by_id(self, id, expected).await
    }

    async fn mark_expired_deprovisioning(&self) -> Result<u64, DomainError> {
        writes::mark_expired_deprovisioning(self).await
    }

    async fn inventory(&self) -> Result<SecretCounts, DomainError> {
        reads::inventory(self).await
    }

    async fn scope_includes_tenant(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<bool, DomainError> {
        reads::scope_includes_tenant(self, scope, tenant).await
    }
}
