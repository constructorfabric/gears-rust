//! Policy and retention-rule intent methods.

use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::policy::{
    PolicyBody, PolicyScope, RetentionRuleBody, RetentionScope, StoredPolicy, StoredRetentionRule,
};
use crate::infra::storage::repo::InsertRetentionRule;
use crate::infra::storage::store::Store;

impl Store {
    /// Fetch the policy for `(policy_scope, scope_owner_id)` within a tenant.
    pub async fn get_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .policies
            .get(&conn, scope, tenant_id, policy_scope, scope_owner_id)
            .await
    }

    /// Upsert (replace) the policy for `(policy_scope, scope_owner_id)`; returns the
    /// new `policy_id`.
    ///
    /// The delete+insert pair runs in one transaction. Concurrent upserts of an
    /// existing row serialize on the DELETE's row lock (the blocked DELETE re-checks
    /// its `WHERE` and removes the just-inserted row). Two concurrent first-time
    /// upserts have nothing to lock; the partial unique indexes make the loser's
    /// `INSERT` fail with `DomainError::Database`.
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

    /// List all retention rules of a tenant (all scopes).
    pub async fn list_retention_rules(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .retention_rules
            .list_for_tenant(&conn, scope, tenant_id)
            .await
    }

    /// Fetch a retention rule by `rule_id`.
    pub async fn get_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<Option<StoredRetentionRule>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos.retention_rules.get(&conn, scope, rule_id).await
    }

    /// Insert a retention rule; returns the new `rule_id`.
    pub async fn insert_retention_rule(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        retention_scope: &RetentionScope,
        scope_target_id: Option<Uuid>,
        body: &RetentionRuleBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
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

    /// Delete a retention rule; `true` if a row was removed.
    pub async fn delete_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .retention_rules
            .delete(&conn, scope, rule_id)
            .await
    }
}
