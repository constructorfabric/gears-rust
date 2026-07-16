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
        // @cpt-begin:cpt-cf-file-storage-flow-policy-get-own:p1:inst-policy-get-authz
        let scope = self
            .authorize_scope_owner(ctx, actions::READ, scope_owner_id)
            .await?;
        // @cpt-end:cpt-cf-file-storage-flow-policy-get-own:p1:inst-policy-get-authz
        // A `User`-scope read with no `scope_owner_id`, or a `Tenant`-scope
        // read with one, is a malformed request that would otherwise either
        // silently resolve to an always-empty row (`None`, rendered as `204
        // No Content`) instead of the `400` the caller needs to fix their
        // request, or query an impossible row shape (tenant policy rows never
        // have an owner). Shared with `validate_policy_body`'s identical
        // check on the write path (`Self::validate_scope_owner_shape`, below).
        Self::validate_scope_owner_shape(&policy_scope, scope_owner_id)?;
        // @cpt-begin:cpt-cf-file-storage-flow-policy-get-own:p1:inst-policy-get-load
        self.store
            .get_policy(
                &scope,
                ctx.subject_tenant_id(),
                &policy_scope,
                scope_owner_id,
            )
            .await
        // @cpt-end:cpt-cf-file-storage-flow-policy-get-own:p1:inst-policy-get-load
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
        // @cpt-begin:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-authz
        let scope = self
            .authorize_scope_owner(ctx, actions::WRITE, scope_owner_id)
            .await?;
        // @cpt-end:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-authz
        // @cpt-begin:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-validate
        Self::validate_policy_body(&policy_scope, scope_owner_id, &body)?;
        // @cpt-end:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-validate
        let now = OffsetDateTime::now_utc();
        let tenant_id = ctx.subject_tenant_id();
        // @cpt-begin:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-upsert
        let policy_id = self
            .store
            .upsert_policy(&scope, tenant_id, &policy_scope, scope_owner_id, &body, now)
            .await?;
        // @cpt-end:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-upsert
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
        // @cpt-begin:cpt-cf-file-storage-flow-policy-get-effective:p1:inst-policy-eff-authz
        let scope = self
            .authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        // @cpt-end:cpt-cf-file-storage-flow-policy-get-effective:p1:inst-policy-eff-authz
        let tenant_id = ctx.subject_tenant_id();

        // @cpt-begin:cpt-cf-file-storage-flow-policy-get-effective:p1:inst-policy-eff-load
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
        // @cpt-end:cpt-cf-file-storage-flow-policy-get-effective:p1:inst-policy-eff-load

        // @cpt-begin:cpt-cf-file-storage-flow-policy-get-effective:p1:inst-policy-eff-resolve
        Ok(PolicyResolver::resolve(
            tenant_policy.as_ref().map(|p| &p.body),
            user_policy.as_ref().map(|p| &p.body),
        ))
        // @cpt-end:cpt-cf-file-storage-flow-policy-get-effective:p1:inst-policy-eff-resolve
    }

    /// List retention rules for the caller's tenant.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_retention_rules(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        // @cpt-begin:cpt-cf-file-storage-flow-retention-list:p1:inst-retention-list-authz
        let scope = self
            .authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        // @cpt-end:cpt-cf-file-storage-flow-retention-list:p1:inst-retention-list-authz
        // @cpt-begin:cpt-cf-file-storage-flow-retention-list:p1:inst-retention-list-load
        self.store
            .list_retention_rules(&scope, ctx.subject_tenant_id())
            .await
        // @cpt-end:cpt-cf-file-storage-flow-retention-list:p1:inst-retention-list-load
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
        // @cpt-begin:cpt-cf-file-storage-flow-retention-create:p1:inst-retention-create-authz
        let scope = self
            .authorize_retention_scope(ctx, &retention_scope, scope_target_id)
            .await?;
        // @cpt-end:cpt-cf-file-storage-flow-retention-create:p1:inst-retention-create-authz
        // @cpt-begin:cpt-cf-file-storage-flow-retention-create:p1:inst-retention-create-validate
        Self::validate_retention_rule(&retention_scope, scope_target_id, &body)?;
        // @cpt-end:cpt-cf-file-storage-flow-retention-create:p1:inst-retention-create-validate
        let now = OffsetDateTime::now_utc();
        let tenant_id = ctx.subject_tenant_id();
        // @cpt-begin:cpt-cf-file-storage-flow-retention-create:p1:inst-retention-create-insert
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
        // @cpt-end:cpt-cf-file-storage-flow-retention-create:p1:inst-retention-create-insert
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
        // the rule's scope/target first — scoped to the caller's own tenant
        // (`Self::tenant_scope`, same prefetch pattern `require_file` uses
        // elsewhere in this gear) rather than `allow_all` — then re-run the
        // same scope-based check `create_retention_rule` uses. Prefetching
        // with `allow_all` would let SecureORM resolve *any* tenant's rule
        // row here, so a foreign tenant's real `rule_id` would reach the
        // authorization check below and 403 there, while a merely-nonexistent
        // id 404s right here — a 403-vs-404 cross-tenant rule-ID oracle, the
        // same class of bug already closed for files. Scoping this fetch to
        // the caller's tenant means a foreign-tenant rule_id simply does not
        // resolve (`None`), so it 404s uniformly with a nonexistent one,
        // before authorization is ever consulted.
        // @cpt-begin:cpt-cf-file-storage-flow-retention-delete:p1:inst-retention-delete-load
        let rule = self
            .store
            .get_retention_rule(&Self::tenant_scope(ctx), rule_id)
            .await?
            .ok_or_else(|| DomainError::retention_rule_not_found(rule_id))?;
        // @cpt-end:cpt-cf-file-storage-flow-retention-delete:p1:inst-retention-delete-load
        // @cpt-begin:cpt-cf-file-storage-flow-retention-delete:p1:inst-retention-delete-authz
        let scope = match self
            .authorize_retention_scope(ctx, &rule.scope, rule.scope_target_id)
            .await
        {
            // A `File`-scope rule whose target file has been deleted (there is
            // no FK/cascade tying `retention_rules.scope_target_id` to
            // `files.file_id`, see the m20260701_000001_p2_initial migration)
            // would otherwise be permanently undeletable: every future call
            // re-resolves the (now-gone) target via `require_file` and 404s
            // before authorization is even attempted. Fall back to the same
            // plain tenant-wide `WRITE` gate the `Tenant`-scope arm uses —
            // there is no file left to check per-file `WRITE` against, so this
            // is the closest equivalent, not a weaker one: the actual DELETE
            // below still runs under the caller's own tenant scope, so a
            // foreign-tenant caller's `delete_retention_rule` still matches
            // zero rows and 404s (never `Forbidden`), staying consistent with
            // how the `Tenant`/`User` arms already behave for a foreign-tenant
            // `rule_id`.
            Err(DomainError::FileNotFound { .. }) if rule.scope == RetentionScope::File => {
                self.authorizer
                    .authorize(ctx, actions::WRITE, "", None)
                    .await?
            }
            other => other?,
        };
        // @cpt-end:cpt-cf-file-storage-flow-retention-delete:p1:inst-retention-delete-authz
        // @cpt-begin:cpt-cf-file-storage-flow-retention-delete:p1:inst-retention-delete-remove
        self.store.delete_retention_rule(&scope, rule_id).await
        // @cpt-end:cpt-cf-file-storage-flow-retention-delete:p1:inst-retention-delete-remove
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
    ///
    /// @cpt-dod:cpt-cf-file-storage-dod-retention-semantic-validation:p2
    fn validate_retention_rule(
        scope: &RetentionScope,
        scope_target_id: Option<Uuid>,
        body: &RetentionRuleBody,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-file-storage-algo-validate-retention-rule:p2:inst-validate-retention-empty
        if body.age.is_none() && body.inactivity.is_none() && body.metadata.is_none() {
            return Err(DomainError::validation(
                "body",
                "retention rule must specify at least one of: age, inactivity, metadata",
            ));
        }
        // @cpt-end:cpt-cf-file-storage-algo-validate-retention-rule:p2:inst-validate-retention-empty
        // @cpt-begin:cpt-cf-file-storage-algo-validate-retention-rule:p2:inst-validate-retention-zero
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
        // @cpt-end:cpt-cf-file-storage-algo-validate-retention-rule:p2:inst-validate-retention-zero
        // @cpt-begin:cpt-cf-file-storage-algo-validate-retention-rule:p2:inst-validate-retention-target
        if matches!(scope, RetentionScope::User | RetentionScope::File) && scope_target_id.is_none()
        {
            return Err(DomainError::validation(
                "scope_target_id",
                "user/file-scope retention rule requires a scope_target_id",
            ));
        }
        // @cpt-end:cpt-cf-file-storage-algo-validate-retention-rule:p2:inst-validate-retention-target
        // @cpt-begin:cpt-cf-file-storage-algo-validate-retention-rule:p2:inst-validate-retention-return
        Ok(())
        // @cpt-end:cpt-cf-file-storage-algo-validate-retention-rule:p2:inst-validate-retention-return
    }

    /// Reject a `(scope, scope_owner_id)` pair whose shape is impossible or
    /// dead, shared by both the read path (`get_own_policy`) and the write
    /// path (`validate_policy_body`).
    ///
    /// - `scope = User` with `scope_owner_id = None`: the effective-policy
    ///   reader (`FileService::get_effective_policy_internal`,
    ///   `create.rs:40-43`) always queries the user-scope row with
    ///   `Some(owner_id)` — a `None`-owner user-scope row can never be read
    ///   back, so on write it is a dead row from the moment it is written,
    ///   and on read it always resolves to `None`/`204` instead of surfacing
    ///   the caller's malformed request as `400`.
    /// - `scope = Tenant` with `scope_owner_id = Some(_)`: tenant policy rows
    ///   never have an owner (`get_policy`/`upsert_policy` key tenant rows on
    ///   `(tenant_id, scope)` alone) — this shape can never match a stored
    ///   row on read, and on write would either be silently ignored or, if
    ///   ever threaded further, query/write an impossible row.
    ///
    /// Shares the `cpt-cf-file-storage-dod-policy-semantic-validation` scope
    /// with `validate_policy_body` below (which carries the marker) rather
    /// than duplicating it here.
    fn validate_scope_owner_shape(
        scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<(), DomainError> {
        match (scope, scope_owner_id) {
            (PolicyScope::User, None) => Err(DomainError::validation(
                "scope_owner_id",
                "user-scope policy requires a scope_owner_id",
            )),
            (PolicyScope::Tenant, Some(_)) => Err(DomainError::validation(
                "scope_owner_id",
                "tenant-scope policy must not carry a scope_owner_id",
            )),
            _ => Ok(()),
        }
    }

    /// Reject a policy body that would be dangerous or dead on write.
    ///
    /// - the `(scope, scope_owner_id)` shape checks from
    ///   `validate_scope_owner_shape` above.
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
    ///
    /// @cpt-dod:cpt-cf-file-storage-dod-policy-semantic-validation:p2
    fn validate_policy_body(
        scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: &PolicyBody,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-file-storage-algo-validate-policy-body:p2:inst-validate-user-owner
        Self::validate_scope_owner_shape(scope, scope_owner_id)?;
        // @cpt-end:cpt-cf-file-storage-algo-validate-policy-body:p2:inst-validate-user-owner
        // @cpt-begin:cpt-cf-file-storage-algo-validate-policy-body:p2:inst-validate-star-slash-star-allowed
        if body.allowed_mime_types.iter().any(|m| m == "*/*") {
            return Err(DomainError::validation(
                "allowed_mime_types",
                "'*/*' is not a valid mime pattern (it silently matches nothing); omit \
                 allowed_mime_types entirely to allow all types",
            ));
        }
        // @cpt-end:cpt-cf-file-storage-algo-validate-policy-body:p2:inst-validate-star-slash-star-allowed
        // @cpt-begin:cpt-cf-file-storage-algo-validate-policy-body:p2:inst-validate-star-slash-star-per-mime
        if body.size_limits.per_mime.iter().any(|o| o.mime == "*/*") {
            return Err(DomainError::validation(
                "size_limits.per_mime",
                "'*/*' is not a valid mime pattern for a per-mime size override; use \
                 size_limits.max_bytes for a global limit instead",
            ));
        }
        // @cpt-end:cpt-cf-file-storage-algo-validate-policy-body:p2:inst-validate-star-slash-star-per-mime
        // @cpt-begin:cpt-cf-file-storage-algo-validate-policy-body:p2:inst-validate-return
        Ok(())
        // @cpt-end:cpt-cf-file-storage-algo-validate-policy-body:p2:inst-validate-return
    }

    // ── authorization helpers ────────────────────────────────────────────────

    /// Shared "try `ADMIN_POLICY` first, else require owner == subject" gate
    /// used by both [`Self::authorize_scope_owner`] (policy read/write) and
    /// the `RetentionScope::User` arm of [`Self::authorize_retention_scope`].
    ///
    /// Tries `ADMIN_POLICY` first (cross-owner / tenant-wide administration);
    /// on `Forbidden`, falls back to `fallback_action` (`READ`/`WRITE`) and
    /// requires `required_owner_id` — when present — to match the caller's
    /// own subject id.
    ///
    /// `required_owner_id == None` is ambiguous between the two callers:
    /// - the policy endpoints use `None` for "tenant scope", which has no
    ///   owner to compare, so the fallback should succeed on
    ///   `fallback_action` alone;
    /// - a `User`-scope retention rule always has a target user, so a missing
    ///   target must be treated as a mismatch, not as "no check".
    ///
    /// `treat_missing_owner_as_authorized` picks between the two.
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
        self.authorize_admin_or_owner(ctx, fallback_action, scope_owner_id, true)
            .await
    }

    /// Authorize a retention-rule mutation (create or delete) for the given
    /// `(retention_scope, scope_target_id)` pair.
    ///
    /// - `Tenant`: stays `WRITE`-gated — there is no owner to compare.
    /// - `User`: requires `scope_target_id == Some(ctx.subject_id())` unless
    ///   the caller holds `ADMIN_POLICY` (unlike [`Self::authorize_scope_owner`],
    ///   a missing target is treated as a mismatch, not as "no check" — a
    ///   `User`-scope retention rule always has a target user).
    /// - `File`: resolves the target file via `require_file`, scoped to the
    ///   caller's own tenant (the same `Self::tenant_scope` prefetch pattern
    ///   `write.rs`/`create.rs` use for ordinary file operations) — a
    ///   foreign-tenant or missing file surfaces as `DomainError::FileNotFound`
    ///   (closing verifier finding B4 *and* a cross-tenant existence oracle: an
    ///   `allow_all` prefetch would resolve a foreign tenant's file, and only
    ///   then fail the *authorization* decision, letting a 201-vs-404 response
    ///   reveal whether a foreign tenant's file UUID exists) — and requires
    ///   per-file `WRITE`, the same check `read_ops.rs`/`write.rs` use for
    ///   ordinary file operations.
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
                    .require_file(&Self::tenant_scope(ctx), target_id)
                    .await?;
                self.authorizer
                    .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(target_id))
                    .await
            }
        }
    }

    /// The caller's own-tenant `AccessScope`, used as the prefetch scope
    /// before a per-action `Authorizer::authorize` decision — mirrors
    /// `FileService::tenant_scope` (`domain/service/mod.rs`) and
    /// `MultipartService`'s private copy of the same helper; kept as its own
    /// copy here rather than shared, following the existing precedent (each
    /// service owns its dependencies independently, see this module's header
    /// doc comment on why `PolicyService` avoids referencing `FileService`).
    fn tenant_scope(ctx: &SecurityContext) -> AccessScope {
        AccessScope::for_tenant(ctx.subject_tenant_id())
    }
}
