//! `SeaORM`-backed implementation of [`SecretRepo`] (ADR-0006).

pub mod helpers;
mod intents;
mod reads;
mod writes;

#[cfg(test)]
mod repo_tests;

use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::{OwnerId, SecretRef, SharingMode, StoreKey, TenantId, ValueVersion};
use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

pub use helpers::{CredstoreDbProvider, SecretRepoImpl};

use crate::domain::error::DomainError;
use crate::domain::secret::model::{
    CleanupTask, Fallback, IntentCommit, NewDeclaredSecret, NewSecret, Reclaimed, SecretRow,
    WriteAttempt,
};
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

    async fn resolve_candidates(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
    ) -> Result<Vec<SecretRow>, DomainError> {
        reads::resolve_candidates(self, req_tenant, subject, key, chain).await
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

    async fn scope_includes_tenant(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<bool, DomainError> {
        Ok(reads::scope_includes_tenant(scope, tenant))
    }

    async fn list_candidate_references(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        chain: &[Uuid],
        reference_in: Option<&[String]>,
        type_scope: &AccessScope,
        cursor: Option<&str>,
        desc: bool,
        limit: u64,
    ) -> Result<Vec<String>, DomainError> {
        reads::list_candidate_references(
            self,
            req_tenant,
            subject,
            chain,
            reference_in,
            type_scope,
            cursor,
            desc,
            limit,
        )
        .await
    }

    async fn list_candidates_for_references(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        chain: &[Uuid],
        references: &[String],
    ) -> Result<Vec<SecretRow>, DomainError> {
        reads::list_candidates_for_references(self, req_tenant, subject, chain, references).await
    }

    async fn begin_write_intent(
        &self,
        attempt: &WriteAttempt,
        lease: Duration,
    ) -> Result<(), DomainError> {
        intents::begin_write_intent(self, attempt, lease).await
    }

    async fn drop_write_intent(&self, attempt_id: Uuid) -> Result<(), DomainError> {
        intents::drop_write_intent(self, attempt_id).await
    }

    async fn insert_active(
        &self,
        scope: &AccessScope,
        new: &NewSecret,
        attempt: &WriteAttempt,
    ) -> Result<IntentCommit<()>, DomainError> {
        writes::insert_active(self, scope, new, attempt).await
    }

    async fn insert_declared(
        &self,
        scope: &AccessScope,
        new: &NewDeclaredSecret,
    ) -> Result<(), DomainError> {
        writes::insert_declared(self, scope, new).await
    }

    async fn switch_value(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: i64,
        sharing: SharingMode,
        fallback: Fallback,
        expires_at: Option<OffsetDateTime>,
        new_value_version: ValueVersion,
        attempt: &WriteAttempt,
    ) -> Result<IntentCommit<SecretRow>, DomainError> {
        writes::switch_value(
            self,
            scope,
            id,
            expected_version,
            sharing,
            fallback,
            expires_at,
            new_value_version,
            attempt,
        )
        .await
    }

    async fn settle_lost_intent(
        &self,
        key: &StoreKey,
        version: &ValueVersion,
        destroy_supported: bool,
    ) -> Result<Vec<CleanupTask>, DomainError> {
        intents::settle_lost_intent(self, key, version, destroy_supported).await
    }

    async fn reclaim_expired(&self, limit: u64) -> Result<Reclaimed, DomainError> {
        intents::reclaim_expired(self, limit).await
    }

    async fn update_metadata(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
        sharing: SharingMode,
        fallback: Fallback,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<Option<SecretRow>, DomainError> {
        writes::update_metadata(
            self,
            scope,
            id,
            expected_version,
            sharing,
            fallback,
            expires_at,
        )
        .await
    }

    async fn remove_value(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
        sharing: SharingMode,
        fallback: Fallback,
        expires_at: Option<OffsetDateTime>,
        destroy_supported: bool,
    ) -> Result<Option<(SecretRow, Vec<CleanupTask>)>, DomainError> {
        writes::remove_value(
            self,
            scope,
            id,
            expected_version,
            sharing,
            fallback,
            expires_at,
            destroy_supported,
        )
        .await
    }

    async fn delete_by_id(
        &self,
        scope: &AccessScope,
        key: &StoreKey,
        expected_version: Option<i64>,
    ) -> Result<(), DomainError> {
        writes::delete_by_id(self, scope, key, expected_version).await
    }
}
