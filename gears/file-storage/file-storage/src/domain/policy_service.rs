//! `PolicyService` — policy and retention-rule administration (read/upsert policy,
//! effective policy, retention rules). Inline policy *enforcement* on core file ops
//! stays in `FileService`.

// Domain terms (ETag, If-Match, FileStorage, GET/PUT) recur throughout the docs.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::authz::{Authorizer, actions};
use crate::domain::error::DomainError;
use crate::domain::policy::{
    EffectivePolicy, PolicyBody, PolicyResolver, PolicyScope, RetentionRuleBody, RetentionScope,
    StoredPolicy, StoredRetentionRule,
};
use crate::domain::ports::PolicyStore;

/// The policy and retention-rule administration service.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct PolicyService {
    store: Arc<dyn PolicyStore>,
    authorizer: Arc<dyn Authorizer>,
}

impl PolicyService {
    pub fn new(store: Arc<dyn PolicyStore>, authorizer: Arc<dyn Authorizer>) -> Self {
        Self { store, authorizer }
    }

    /// Get the raw (own-level) policy body for a scope, if one has been set.
    pub async fn get_own_policy(
        &self,
        ctx: &SecurityContext,
        policy_scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError> {
        let scope = self
            .authorize_scope_owner(ctx, actions::READ, scope_owner_id)
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
    pub async fn set_policy(
        &self,
        ctx: &SecurityContext,
        policy_scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: PolicyBody,
    ) -> Result<StoredPolicy, DomainError> {
        // Tenant scope (`scope_owner_id == None`) has no owner to compare; plain `WRITE` gates it.
        let scope = self
            .authorize_scope_owner(ctx, actions::WRITE, scope_owner_id)
            .await?;
        Self::validate_policy_body(&policy_scope, scope_owner_id, &body)?;
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

    /// Effective policy for the caller: tenant and user levels, most-restrictive-wins.
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
    pub async fn create_retention_rule(
        &self,
        ctx: &SecurityContext,
        retention_scope: RetentionScope,
        scope_target_id: Option<Uuid>,
        body: RetentionRuleBody,
    ) -> Result<StoredRetentionRule, DomainError> {
        let scope = self
            .authorize_retention_scope(ctx, &retention_scope, scope_target_id)
            .await?;
        Self::validate_retention_rule(&retention_scope, scope_target_id, &body)?;
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
    pub async fn delete_retention_rule(
        &self,
        ctx: &SecurityContext,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        // A bare `rule_id` carries no ownership, so fetch the rule (via `allow_all`, only to
        // decide authorization) and re-run the scope check `create_retention_rule` uses.
        let rule = self
            .store
            .get_retention_rule(&AccessScope::allow_all(), rule_id)
            .await?
            .ok_or_else(|| DomainError::retention_rule_not_found(rule_id))?;
        let scope = self
            .authorize_retention_scope(ctx, &rule.scope, rule.scope_target_id)
            .await?;
        self.store.delete_retention_rule(&scope, rule_id).await
    }

    /// Reject a retention-rule body that is dead or dangerous on write: no criteria, a
    /// zero `age`/`inactivity` day count (matches every file in the tenant on the next
    /// sweep run, deleting rows and blobs irreversibly), or `user`/`file` scope without a
    /// target (also closes the gap for `ADMIN_POLICY` callers in `authorize_retention_scope`).
    fn validate_retention_rule(
        scope: &RetentionScope,
        scope_target_id: Option<Uuid>,
        body: &RetentionRuleBody,
    ) -> Result<(), DomainError> {
        if body.age.is_none() && body.inactivity.is_none() && body.metadata.is_none() {
            return Err(DomainError::validation(
                "body",
                "retention rule must specify at least one of: age, inactivity, metadata",
            ));
        }
        if let Some(age) = &body.age
            && age.max_age_days < 1
        {
            return Err(DomainError::validation(
                "age.max_age_days",
                "must be >= 1 (0 would match every file in the tenant immediately)",
            ));
        }
        if let Some(inactivity) = &body.inactivity
            && inactivity.inactivity_days < 1
        {
            return Err(DomainError::validation(
                "inactivity.inactivity_days",
                "must be >= 1 (0 would match every file in the tenant immediately)",
            ));
        }
        if matches!(scope, RetentionScope::User | RetentionScope::File) && scope_target_id.is_none()
        {
            return Err(DomainError::validation(
                "scope_target_id",
                "user/file-scope retention rule requires a scope_target_id",
            ));
        }
        Ok(())
    }

    /// Reject a policy body that is dead or dangerous on write.
    ///
    /// - `User` scope without `scope_owner_id`: the reader always queries with
    ///   `Some(owner_id)`, so such a row could never be read back.
    /// - `*/*` in `allowed_mime_types` or `size_limits.per_mime`: the matcher only
    ///   handles `type/*`, so `*/*` silently matches nothing (an accidental deny-all).
    ///   Callers wanting no restriction should omit the entry.
    fn validate_policy_body(
        scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: &PolicyBody,
    ) -> Result<(), DomainError> {
        if matches!(scope, PolicyScope::User) && scope_owner_id.is_none() {
            return Err(DomainError::validation(
                "scope_owner_id",
                "user-scope policy requires a scope_owner_id",
            ));
        }
        if body.allowed_mime_types.iter().any(|m| m == "*/*") {
            return Err(DomainError::validation(
                "allowed_mime_types",
                "'*/*' is not a valid mime pattern (it silently matches nothing); omit \
                 allowed_mime_types entirely to allow all types",
            ));
        }
        if body.size_limits.per_mime.iter().any(|o| o.mime == "*/*") {
            return Err(DomainError::validation(
                "size_limits.per_mime",
                "'*/*' is not a valid mime pattern for a per-mime size override; use \
                 size_limits.max_bytes for a global limit instead",
            ));
        }
        Ok(())
    }

    /// Try `ADMIN_POLICY` (cross-owner / tenant-wide); on `Forbidden`, fall back to
    /// `fallback_action` (`READ`/`WRITE`) and require `required_owner_id`, when present, to
    /// equal the caller's subject id. A missing owner is "tenant scope" for the policy
    /// endpoints (authorized by the fallback alone) but a mismatch for `User`-scope retention
    /// rules; `treat_missing_owner_as_authorized` picks between the two.
    async fn authorize_admin_or_owner(
        &self,
        ctx: &SecurityContext,
        fallback_action: &str,
        required_owner_id: Option<Uuid>,
        treat_missing_owner_as_authorized: bool,
    ) -> Result<AccessScope, DomainError> {
        match self
            .authorizer
            .authorize(ctx, actions::ADMIN_POLICY, "", None)
            .await
        {
            Ok(scope) => Ok(scope),
            Err(DomainError::Forbidden) => {
                let scope = self
                    .authorizer
                    .authorize(ctx, fallback_action, "", None)
                    .await?;
                let is_owner = match required_owner_id {
                    Some(owner_id) => owner_id == ctx.subject_id(),
                    None => treat_missing_owner_as_authorized,
                };
                if !is_owner {
                    return Err(DomainError::Forbidden);
                }
                Ok(scope)
            }
            Err(err) => Err(err),
        }
    }

    /// Policy read/write gate: `ADMIN_POLICY`, else the fallback action on the caller's own
    /// scope (`None` owner = tenant scope, authorized by the fallback alone).
    async fn authorize_scope_owner(
        &self,
        ctx: &SecurityContext,
        fallback_action: &str,
        scope_owner_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        self.authorize_admin_or_owner(ctx, fallback_action, scope_owner_id, true)
            .await
    }

    /// Authorize a retention-rule mutation for `(retention_scope, scope_target_id)`.
    ///
    /// - `Tenant`: `WRITE`.
    /// - `User`: target must be the caller unless they hold `ADMIN_POLICY`; a missing target
    ///   is a mismatch.
    /// - `File`: the target file must resolve (missing/foreign yields `FileNotFound`) and
    ///   the caller needs per-file `WRITE`.
    async fn authorize_retention_scope(
        &self,
        ctx: &SecurityContext,
        retention_scope: &RetentionScope,
        scope_target_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        match retention_scope {
            RetentionScope::Tenant => {
                self.authorizer
                    .authorize(ctx, actions::WRITE, "", None)
                    .await
            }
            RetentionScope::User => {
                self.authorize_admin_or_owner(ctx, actions::WRITE, scope_target_id, false)
                    .await
            }
            RetentionScope::File => {
                let target_id = scope_target_id.ok_or_else(|| DomainError::Validation {
                    field: "scope_target_id".to_owned(),
                    message: "file-scope retention rule requires scope_target_id".to_owned(),
                })?;
                let file = self
                    .store
                    .require_file(&AccessScope::allow_all(), target_id)
                    .await?;
                self.authorizer
                    .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(target_id))
                    .await
            }
        }
    }
}
