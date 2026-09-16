//! Policy and retention-rule intent methods.

use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::policy::{
    PolicyBody, PolicyScope, RetentionRuleBody, RetentionScope, StoredPolicy, StoredRetentionRule,
};
use crate::infra::storage::db::db_err;
use crate::infra::storage::repo::InsertRetentionRule;
use crate::infra::storage::store::Store;

impl Store {
    // ── policy store ──────────────────────────────────────────────────────────

    /// Fetch the policy for a given `(policy_scope, scope_owner_id)` within a
    /// tenant. Returns `None` when no policy has been configured for that scope.
    pub async fn get_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .policies
            .get(&conn, scope, tenant_id, policy_scope, scope_owner_id)
            .await
    }

    /// Upsert (replace) the policy for a given `(policy_scope, scope_owner_id)`.
    /// Returns the new `policy_id`.
    ///
    /// `PolicyRepo::upsert` internally does a `delete_many()` followed by an
    /// independent `insert`. Run outside any transaction, this leaves a
    /// window where two concurrent callers for the same scope could each see
    /// nothing to delete and both insert, corrupting the
    /// at-most-one-row-per-scope invariant. Wrapping the pair in an explicit
    /// DB transaction here (same
    /// `self.db.db().transaction_ref_mapped(...)` pattern
    /// `rebind_version_backend` uses in `store/versions.rs`) serializes
    /// concurrent upserts for an *existing* row via the DELETE's row lock: a
    /// blocked concurrent DELETE re-evaluates its `WHERE` clause once the
    /// first transaction commits and ends up deleting the just-inserted row
    /// instead of matching nothing, so exactly one row survives. The
    /// remaining gap — two concurrent *first-time* upserts, where neither
    /// transaction's DELETE has anything to lock — is closed by the new
    /// `policies_user_scope_unique_idx` / `policies_tenant_scope_unique_idx`
    /// partial unique indexes (migration `m20260706_000003`): the losing
    /// writer's `INSERT` now fails with a unique-constraint violation
    /// (`DomainError::Database`) instead of silently creating a duplicate
    /// row. This keeps `PolicyRepo::upsert` itself, and its existing
    /// `SecureORM` scope validation on both the delete and the insert,
    /// completely unchanged.
    pub async fn upsert_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: &PolicyBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        let policies = self.repos.policies.clone();
        let scope = scope.clone();
        let policy_scope = policy_scope.clone();
        let body = body.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    policies
                        .upsert(
                            tx,
                            &scope,
                            tenant_id,
                            &policy_scope,
                            scope_owner_id,
                            &body,
                            now,
                        )
                        .await
                })
            })
            .await
    }

    /// List all retention rules for a tenant (all scopes).
    pub async fn list_retention_rules(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .retention_rules
            .list_for_tenant(&conn, scope, tenant_id)
            .await
    }

    /// Fetch a single retention rule by `rule_id`.
    pub async fn get_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<Option<StoredRetentionRule>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.retention_rules.get(&conn, scope, rule_id).await
    }

    /// Insert a new retention rule. Returns the assigned `rule_id`.
    pub async fn insert_retention_rule(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        retention_scope: &RetentionScope,
        scope_target_id: Option<Uuid>,
        body: &RetentionRuleBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .retention_rules
            .insert(
                &conn,
                scope,
                InsertRetentionRule {
                    tenant_id,
                    retention_scope,
                    scope_target_id,
                    body,
                    now,
                },
            )
            .await
    }

    /// Delete a retention rule by `rule_id`. Returns `true` if a row was removed.
    pub async fn delete_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .retention_rules
            .delete(&conn, scope, rule_id)
            .await
    }
}
