//! `PolicyService` — policy and retention-rule administration.
//!
//! Owns the P2-M1 flows: read/upsert policy for tenant and user scopes,
//! compute effective policy, and manage retention rules. Extracted from
//! `FileService` to reduce its Henry-Kafura coupling score.
//!
//! `PolicyService` holds its own copies of the shared dependencies (`Store`
//! via `PolicyStore`, `Authorizer`) so it does NOT reference `FileService` —
//! that keeps the fan-in graph clean and avoids raising the HK score of
//! `FileService`.
//!
//! The inline policy *enforcement* used by core file ops (create/finalize/bind/
//! update_metadata) stays in `FileService` — only the standalone admin/management
//! surface moves here.

// Domain terms (ETag, If-Match, FileStorage, GET/PUT) recur throughout the docs.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::authz::{Authorizer, actions};
use crate::domain::error::DomainError;
use crate::domain::policy::{
    EffectivePolicy, PolicyBody, PolicyResolver, PolicyScope, RetentionRuleBody, RetentionScope,
    StoredPolicy, StoredRetentionRule,
};
use crate::domain::ports::PolicyStore;

/// The policy and retention-rule administration service (P2-M1).
///
/// Extracted from `FileService` to reduce its Henry-Kafura coupling score.
/// All standalone policy and retention-rule operations live here; the struct
/// is wired alongside `FileService` in `gear.rs` and served under the same
/// REST prefix.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct PolicyService {
    store: Arc<dyn PolicyStore>,
    authorizer: Arc<dyn Authorizer>,
}

impl PolicyService {
    pub fn new(store: Arc<dyn PolicyStore>, authorizer: Arc<dyn Authorizer>) -> Self {
        Self { store, authorizer }
    }

    // ── policy management (P2-M1) ─────────────────────────────────────────────

    /// Get the raw (own-level) policy body for a scope, if one has been set.
    ///
    /// @cpt-cf-file-storage-usecase-configure-policy
    pub async fn get_own_policy(
        &self,
        ctx: &SecurityContext,
        policy_scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        self.store
            .get_policy(
                &scope,
                ctx.subject_tenant_id(),
                &policy_scope,
                scope_owner_id,
            )
            .await
    }

    /// Set (upsert) the policy for a scope. Tenant-level policy requires the
    /// caller to have appropriate authorization; user-level is self-service.
    ///
    /// @cpt-cf-file-storage-usecase-configure-policy
    pub async fn set_policy(
        &self,
        ctx: &SecurityContext,
        policy_scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: PolicyBody,
    ) -> Result<StoredPolicy, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, "", None)
            .await?;
        let now = OffsetDateTime::now_utc();
        let tenant_id = ctx.subject_tenant_id();
        let policy_id = self
            .store
            .upsert_policy(&scope, tenant_id, &policy_scope, scope_owner_id, &body, now)
            .await?;
        Ok(StoredPolicy {
            policy_id,
            tenant_id,
            scope: policy_scope,
            scope_owner_id,
            body,
            // The upsert wrote both timestamps to `now`.
            created_at: now,
            updated_at: now,
        })
    }

    /// Compute the effective policy for the current caller context, combining
    /// the tenant-level and user-level policies with most-restrictive-wins.
    ///
    /// @cpt-cf-file-storage-usecase-configure-policy
    /// @cpt-cf-file-storage-fr-allowed-types-policy
    /// @cpt-cf-file-storage-fr-size-limits-policy
    /// @cpt-cf-file-storage-fr-metadata-limits
    pub async fn get_effective_policy(
        &self,
        ctx: &SecurityContext,
        user_owner_id: Option<Uuid>,
    ) -> Result<EffectivePolicy, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        let tenant_id = ctx.subject_tenant_id();

        let tenant_policy = self
            .store
            .get_policy(&scope, tenant_id, &PolicyScope::Tenant, None)
            .await?;
        let user_policy = match user_owner_id {
            Some(uid) => {
                self.store
                    .get_policy(&scope, tenant_id, &PolicyScope::User, Some(uid))
                    .await?
            }
            None => None,
        };

        Ok(PolicyResolver::resolve(
            tenant_policy.as_ref().map(|p| &p.body),
            user_policy.as_ref().map(|p| &p.body),
        ))
    }

    /// List retention rules for the caller's tenant.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_retention_rules(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        self.store
            .list_retention_rules(&scope, ctx.subject_tenant_id())
            .await
    }

    /// Create a new retention rule.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn create_retention_rule(
        &self,
        ctx: &SecurityContext,
        retention_scope: RetentionScope,
        scope_target_id: Option<Uuid>,
        body: RetentionRuleBody,
    ) -> Result<StoredRetentionRule, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, "", None)
            .await?;
        let now = OffsetDateTime::now_utc();
        let tenant_id = ctx.subject_tenant_id();
        let rule_id = self
            .store
            .insert_retention_rule(
                &scope,
                tenant_id,
                &retention_scope,
                scope_target_id,
                &body,
                now,
            )
            .await?;
        Ok(StoredRetentionRule {
            rule_id,
            tenant_id,
            scope: retention_scope,
            scope_target_id,
            body,
            created_at: now,
        })
    }

    /// Delete a retention rule by `rule_id`.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn delete_retention_rule(
        &self,
        ctx: &SecurityContext,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        let scope = self
            .authorizer
            .authorize(ctx, actions::DELETE, "", None)
            .await?;
        self.store.delete_retention_rule(&scope, rule_id).await
    }
}
