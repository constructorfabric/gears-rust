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
use toolkit_security::{AccessScope, SecurityContext};
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
    ///
    /// @cpt-cf-file-storage-usecase-configure-policy
    pub async fn set_policy(
        &self,
        ctx: &SecurityContext,
        policy_scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: PolicyBody,
    ) -> Result<StoredPolicy, DomainError> {
        // Tenant-scope requests (`scope_owner_id == None`) stay gated on plain
        // `WRITE` — there is no "owner" to compare at tenant scope. Tightening
        // tenant-scope writes to require `ADMIN_POLICY` as well is a follow-up
        // the team may choose to make; not mandated here.
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
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn delete_retention_rule(
        &self,
        ctx: &SecurityContext,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        // Fetch-then-reauthorize: a bare `rule_id` carries no ownership
        // information, so the coarse `DELETE, "", None` check alone would let
        // any tenant member delete any other member's retention rule. Resolve
        // the rule's scope/target first (via `allow_all` — this is a read used
        // only to make the authorization decision below, mirroring the
        // `require_file` prefetch pattern already used elsewhere in this gear),
        // then re-run the same scope-based check `create_retention_rule` uses.
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

    // ── semantic validation (P2 remediation 0.11) ───────────────────────────────

    /// Reject a retention-rule body that would be dangerous or dead on write,
    /// rather than letting it be silently accepted and later executed (or
    /// silently never executed) by the sweep.
    ///
    /// - All of `age`/`inactivity`/`metadata` `None`: the rule can never match
    ///   any file — almost certainly a mistake.
    /// - `age.max_age_days == 0` or `inactivity.inactivity_days == 0`: matches
    ///   *every* file in the tenant on the very next sweep tick (the age check in
    ///   `cleanup.rs`'s `rule_matches` is `now - created_at > Duration::days(0)`,
    ///   true for any file at all), permanently deleting rows **and** blobs with
    ///   no dry-run and no undo. If an "expire everything now" operation is ever
    ///   a real need, it must be an explicit, separately-authorized admin
    ///   action — never a normal retention rule.
    /// - `scope` ∈ {`user`, `file`} with `scope_target_id = None`: a dead rule
    ///   that can never resolve to a target file. `File`-scope already fails
    ///   earlier in `authorize_retention_scope` (which requires the target to
    ///   resolve a real file), but `User`-scope only rejects a missing target
    ///   for non-`ADMIN_POLICY` callers, so this closes the same gap for an
    ///   admin caller.
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

    /// Reject a policy body that would be dangerous or dead on write.
    ///
    /// - `scope = User` with `scope_owner_id = None`: the effective-policy
    ///   reader (`FileService::get_effective_policy_internal`,
    ///   `create.rs:40-43`) always queries the user-scope row with
    ///   `Some(owner_id)` — a `None`-owner user-scope row can never be read
    ///   back, so it is a dead row from the moment it is written.
    /// - a `*/*` entry in `allowed_mime_types` or `size_limits.per_mime`: the
    ///   wildcard matcher (`PolicyResolver::mime_allowed`) only special-cases
    ///   the *subtype* half of a pattern (`"image/*"`), so `*/*` splits into
    ///   `pt = "*"`, and `pt == mt` is never true for a real mime type — it
    ///   silently matches nothing, acting as an accidental deny-all rather
    ///   than the "allow everything" the caller almost certainly intended.
    ///   Rejected outright (simpler and safer than teaching the matcher a
    ///   second wildcard meaning): a caller that wants "no restriction" should
    ///   omit `allowed_mime_types` entirely (`None`/empty already means
    ///   unrestricted), and a caller that wants "no per-mime override" should
    ///   omit the `per_mime` entry.
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

    // ── authorization helpers ────────────────────────────────────────────────

    /// Try `ADMIN_POLICY` first (cross-owner / tenant-wide administration); on
    /// `Forbidden`, fall back to `fallback_action` (`READ`/`WRITE`) and require
    /// `scope_owner_id` — when present — to match the caller's own subject id.
    /// `scope_owner_id == None` means "tenant scope" for the policy endpoints,
    /// which has no owner to compare, so the fallback succeeds on
    /// `fallback_action` alone in that case.
    async fn authorize_scope_owner(
        &self,
        ctx: &SecurityContext,
        fallback_action: &str,
        scope_owner_id: Option<Uuid>,
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
                if let Some(scope_owner_id) = scope_owner_id
                    && scope_owner_id != ctx.subject_id()
                {
                    return Err(DomainError::Forbidden);
                }
                Ok(scope)
            }
            Err(err) => Err(err),
        }
    }

    /// Authorize a retention-rule mutation (create or delete) for the given
    /// `(retention_scope, scope_target_id)` pair.
    ///
    /// - `Tenant`: stays `WRITE`-gated — there is no owner to compare.
    /// - `User`: requires `scope_target_id == Some(ctx.subject_id())` unless
    ///   the caller holds `ADMIN_POLICY` (unlike [`Self::authorize_scope_owner`],
    ///   a missing target is treated as a mismatch, not as "no check" — a
    ///   `User`-scope retention rule always has a target user).
    /// - `File`: resolves the target file via `require_file` (a missing/foreign
    ///   file surfaces as `DomainError::FileNotFound`, closing verifier finding
    ///   B4) and requires per-file `WRITE`, the same check `read_ops.rs`/
    ///   `write.rs` use for ordinary file operations.
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
            RetentionScope::User => match self
                .authorizer
                .authorize(ctx, actions::ADMIN_POLICY, "", None)
                .await
            {
                Ok(scope) => Ok(scope),
                Err(DomainError::Forbidden) => {
                    let scope = self
                        .authorizer
                        .authorize(ctx, actions::WRITE, "", None)
                        .await?;
                    if scope_target_id != Some(ctx.subject_id()) {
                        return Err(DomainError::Forbidden);
                    }
                    Ok(scope)
                }
                Err(err) => Err(err),
            },
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
