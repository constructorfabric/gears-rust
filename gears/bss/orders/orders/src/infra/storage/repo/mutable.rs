//! Conditional updates of mutable working rows and operational ledgers.
use super::{AccessScope, LockedOrder, ScopeError, SecureInsertExt, TransactionRunner, Uuid};
use crate::infra::storage::entity;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, Iterable, ModelTrait,
    QueryFilter, QuerySelect,
};
use toolkit_db::secure::{SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
impl<T: TransactionRunner> LockedOrder<'_, T> {
    pub(crate) async fn replace_commercial_attempt(
        &self,
        expected: &entity::commercial_attempt::Model,
        proposed: entity::commercial_attempt::Model,
    ) -> Result<(), ScopeError> {
        let tx = self.tx;
        let scope = self.child_scope();
        let scope = &scope;
        if proposed.order_id != self.row.order_id {
            return Err(ScopeError::Denied("foreign parent"));
        }
        if proposed.attempt_id != expected.attempt_id {
            return Err(ScopeError::Denied("immutable key"));
        }
        let stored = entity::commercial_attempt::Entity::find_by_id(expected.attempt_id)
            .lock_exclusive()
            .secure()
            .scope_with(scope)
            .one(tx)
            .await?
            .ok_or(ScopeError::Denied("hidden record"))?;
        if &stored != expected {
            return Err(ScopeError::Denied("stale storage facts or ownership fence"));
        }
        let complete = proposed.clone().into_active_model();
        entity::commercial_attempt::Entity::insert(complete.clone())
            .secure()
            .scope_with_model(scope, &complete)?;
        let mut active = stored.clone().into_active_model();
        let mut changed = false;
        for col in entity::commercial_attempt::Column::iter() {
            if stored.get(col) != proposed.get(col) {
                active.set(col, proposed.get(col));
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        let result = entity::commercial_attempt::Entity::update_many()
            .set(active)
            .filter(entity::commercial_attempt::Column::AttemptId.eq(expected.attempt_id))
            .secure()
            .scope_with(scope)
            .exec(tx)
            .await?;
        if result.rows_affected != 1 {
            return Err(ScopeError::Denied("conditional update lost"));
        }
        Ok(())
    }
}
impl<T: TransactionRunner> LockedOrder<'_, T> {
    pub(crate) async fn replace_draft_content(
        &self,
        expected: &entity::draft_content::Model,
        proposed: entity::draft_content::Model,
    ) -> Result<(), ScopeError> {
        let tx = self.tx;
        let scope = self.child_scope();
        let scope = &scope;
        if proposed.order_id != self.row.order_id {
            return Err(ScopeError::Denied("foreign parent"));
        }
        if proposed.order_id != expected.order_id || proposed.line_id != expected.line_id {
            return Err(ScopeError::Denied("immutable key"));
        }
        let stored =
            entity::draft_content::Entity::find_by_id((expected.order_id, expected.line_id))
                .lock_exclusive()
                .secure()
                .scope_with(scope)
                .one(tx)
                .await?
                .ok_or(ScopeError::Denied("hidden record"))?;
        if &stored != expected {
            return Err(ScopeError::Denied("stale storage facts or ownership fence"));
        }
        let complete = proposed.clone().into_active_model();
        entity::draft_content::Entity::insert(complete.clone())
            .secure()
            .scope_with_model(scope, &complete)?;
        let mut active = stored.clone().into_active_model();
        let mut changed = false;
        for col in entity::draft_content::Column::iter() {
            if stored.get(col) != proposed.get(col) {
                active.set(col, proposed.get(col));
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        let result = entity::draft_content::Entity::update_many()
            .set(active)
            .filter(entity::draft_content::Column::OrderId.eq(expected.order_id))
            .filter(entity::draft_content::Column::LineId.eq(expected.line_id))
            .secure()
            .scope_with(scope)
            .exec(tx)
            .await?;
        if result.rows_affected != 1 {
            return Err(ScopeError::Denied("conditional update lost"));
        }
        Ok(())
    }
}
impl<T: TransactionRunner> LockedOrder<'_, T> {
    pub(crate) async fn replace_order_admin(
        &self,
        expected: &entity::order_admin::Model,
        proposed: entity::order_admin::Model,
    ) -> Result<(), ScopeError> {
        let tx = self.tx;
        let scope = self.child_scope();
        let scope = &scope;
        if proposed.order_id != self.row.order_id {
            return Err(ScopeError::Denied("foreign parent"));
        }
        if proposed.order_id != expected.order_id {
            return Err(ScopeError::Denied("immutable key"));
        }
        let stored = entity::order_admin::Entity::find_by_id(expected.order_id)
            .lock_exclusive()
            .secure()
            .scope_with(scope)
            .one(tx)
            .await?
            .ok_or(ScopeError::Denied("hidden record"))?;
        if &stored != expected {
            return Err(ScopeError::Denied("stale storage facts or ownership fence"));
        }
        let complete = proposed.clone().into_active_model();
        entity::order_admin::Entity::insert(complete.clone())
            .secure()
            .scope_with_model(scope, &complete)?;
        let mut active = stored.clone().into_active_model();
        let mut changed = false;
        for col in entity::order_admin::Column::iter() {
            if stored.get(col) != proposed.get(col) {
                active.set(col, proposed.get(col));
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        let result = entity::order_admin::Entity::update_many()
            .set(active)
            .filter(entity::order_admin::Column::OrderId.eq(expected.order_id))
            .secure()
            .scope_with(scope)
            .exec(tx)
            .await?;
        if result.rows_affected != 1 {
            return Err(ScopeError::Denied("conditional update lost"));
        }
        Ok(())
    }
}
impl<T: TransactionRunner> LockedOrder<'_, T> {
    pub(crate) async fn replace_order_line_admin(
        &self,
        expected: &entity::order_line_admin::Model,
        proposed: entity::order_line_admin::Model,
    ) -> Result<(), ScopeError> {
        let tx = self.tx;
        let scope = self.child_scope();
        let scope = &scope;
        if proposed.order_id != self.row.order_id {
            return Err(ScopeError::Denied("foreign parent"));
        }
        if proposed.order_id != expected.order_id || proposed.line_id != expected.line_id {
            return Err(ScopeError::Denied("immutable key"));
        }
        let stored =
            entity::order_line_admin::Entity::find_by_id((expected.order_id, expected.line_id))
                .lock_exclusive()
                .secure()
                .scope_with(scope)
                .one(tx)
                .await?
                .ok_or(ScopeError::Denied("hidden record"))?;
        if &stored != expected {
            return Err(ScopeError::Denied("stale storage facts or ownership fence"));
        }
        let complete = proposed.clone().into_active_model();
        entity::order_line_admin::Entity::insert(complete.clone())
            .secure()
            .scope_with_model(scope, &complete)?;
        let mut active = stored.clone().into_active_model();
        let mut changed = false;
        for col in entity::order_line_admin::Column::iter() {
            if stored.get(col) != proposed.get(col) {
                active.set(col, proposed.get(col));
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        let result = entity::order_line_admin::Entity::update_many()
            .set(active)
            .filter(entity::order_line_admin::Column::OrderId.eq(expected.order_id))
            .filter(entity::order_line_admin::Column::LineId.eq(expected.line_id))
            .secure()
            .scope_with(scope)
            .exec(tx)
            .await?;
        if result.rows_affected != 1 {
            return Err(ScopeError::Denied("conditional update lost"));
        }
        Ok(())
    }
}
// Overlap claims have no generic replace: `release_claims` below is their only mutation, so
// `released_at` is always database time, exact-ID and count-checked (DESIGN §3.7, OL-2).
impl<T: TransactionRunner> LockedOrder<'_, T> {
    pub(crate) async fn replace_fulfillment_control(
        &self,
        expected: &entity::fulfillment_control::Model,
        proposed: entity::fulfillment_control::Model,
    ) -> Result<(), ScopeError> {
        let tx = self.tx;
        let scope = self.child_scope();
        let scope = &scope;
        if proposed.order_id != self.row.order_id {
            return Err(ScopeError::Denied("foreign parent"));
        }
        if proposed.control_id != expected.control_id {
            return Err(ScopeError::Denied("immutable key"));
        }
        let stored = entity::fulfillment_control::Entity::find_by_id(expected.control_id)
            .lock_exclusive()
            .secure()
            .scope_with(scope)
            .one(tx)
            .await?
            .ok_or(ScopeError::Denied("hidden record"))?;
        if &stored != expected {
            return Err(ScopeError::Denied("stale storage facts or ownership fence"));
        }
        let complete = proposed.clone().into_active_model();
        entity::fulfillment_control::Entity::insert(complete.clone())
            .secure()
            .scope_with_model(scope, &complete)?;
        let mut active = stored.clone().into_active_model();
        let mut changed = false;
        for col in entity::fulfillment_control::Column::iter() {
            if stored.get(col) != proposed.get(col) {
                active.set(col, proposed.get(col));
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        let result = entity::fulfillment_control::Entity::update_many()
            .set(active)
            .filter(entity::fulfillment_control::Column::ControlId.eq(expected.control_id))
            .secure()
            .scope_with(scope)
            .exec(tx)
            .await?;
        if result.rows_affected != 1 {
            return Err(ScopeError::Denied("conditional update lost"));
        }
        Ok(())
    }
}
pub async fn replace_idempotency(
    tx: &impl TransactionRunner,
    scope: &AccessScope,
    expected: &entity::idempotency::Model,
    proposed: entity::idempotency::Model,
) -> Result<(), ScopeError> {
    if proposed.operation != expected.operation
        || proposed.principal_scope != expected.principal_scope
        || proposed.idempotency_key != expected.idempotency_key
    {
        return Err(ScopeError::Denied("immutable key"));
    }
    let stored = entity::idempotency::Entity::find_by_id((
        expected.operation.clone(),
        expected.principal_scope.clone(),
        expected.idempotency_key.clone(),
    ))
    .lock_exclusive()
    .secure()
    .scope_with(scope)
    .one(tx)
    .await?
    .ok_or(ScopeError::Denied("hidden record"))?;
    if &stored != expected {
        return Err(ScopeError::Denied("stale storage facts or ownership fence"));
    }
    let complete = proposed.clone().into_active_model();
    entity::idempotency::Entity::insert(complete.clone())
        .secure()
        .scope_with_model(scope, &complete)?;
    let mut active = stored.clone().into_active_model();
    let mut changed = false;
    for col in entity::idempotency::Column::iter() {
        if stored.get(col) != proposed.get(col) {
            active.set(col, proposed.get(col));
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    let result = entity::idempotency::Entity::update_many()
        .set(active)
        .filter(entity::idempotency::Column::Operation.eq(expected.operation.clone()))
        .filter(entity::idempotency::Column::PrincipalScope.eq(expected.principal_scope.clone()))
        .filter(entity::idempotency::Column::IdempotencyKey.eq(expected.idempotency_key.clone()))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await?;
    if result.rows_affected != 1 {
        return Err(ScopeError::Denied("conditional update lost"));
    }
    Ok(())
}
impl<T: TransactionRunner> LockedOrder<'_, T> {
    pub(crate) async fn replace_line_fulfillment(
        &self,
        expected: &entity::line_fulfillment::Model,
        proposed: entity::line_fulfillment::Model,
    ) -> Result<(), ScopeError> {
        let tx = self.tx;
        let scope = self.child_scope();
        let scope = &scope;
        if proposed.order_id != self.row.order_id {
            return Err(ScopeError::Denied("foreign parent"));
        }
        if proposed.order_id != expected.order_id || proposed.line_id != expected.line_id {
            return Err(ScopeError::Denied("immutable key"));
        }
        let stored =
            entity::line_fulfillment::Entity::find_by_id((expected.order_id, expected.line_id))
                .lock_exclusive()
                .secure()
                .scope_with(scope)
                .one(tx)
                .await?
                .ok_or(ScopeError::Denied("hidden record"))?;
        if &stored != expected {
            return Err(ScopeError::Denied("stale storage facts or ownership fence"));
        }
        let complete = proposed.clone().into_active_model();
        entity::line_fulfillment::Entity::insert(complete.clone())
            .secure()
            .scope_with_model(scope, &complete)?;
        let mut active = stored.clone().into_active_model();
        let mut changed = false;
        for col in entity::line_fulfillment::Column::iter() {
            if stored.get(col) != proposed.get(col) {
                active.set(col, proposed.get(col));
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        let result = entity::line_fulfillment::Entity::update_many()
            .set(active)
            .filter(entity::line_fulfillment::Column::OrderId.eq(expected.order_id))
            .filter(entity::line_fulfillment::Column::LineId.eq(expected.line_id))
            .secure()
            .scope_with(scope)
            .exec(tx)
            .await?;
        if result.rows_affected != 1 {
            return Err(ScopeError::Denied("conditional update lost"));
        }
        Ok(())
    }
}
pub async fn replace_date_policy(
    tx: &impl TransactionRunner,
    scope: &AccessScope,
    expected: &entity::date_policy::Model,
    proposed: entity::date_policy::Model,
) -> Result<(), ScopeError> {
    if proposed.policy_id != expected.policy_id {
        return Err(ScopeError::Denied("immutable key"));
    }
    entity::date_policy::Entity::find()
        .filter(entity::date_policy::Column::ResourceTenantId.is_null())
        .lock_exclusive()
        .secure()
        .scope_with(scope)
        .one(tx)
        .await?
        .ok_or(ScopeError::Denied(
            "platform policy serialization scope required",
        ))?;
    let stored = entity::date_policy::Entity::find_by_id(expected.policy_id)
        .lock_exclusive()
        .secure()
        .scope_with(scope)
        .one(tx)
        .await?
        .ok_or(ScopeError::Denied("hidden record"))?;
    if &stored != expected {
        return Err(ScopeError::Denied("stale storage facts or ownership fence"));
    }
    let complete = proposed.clone().into_active_model();
    entity::date_policy::Entity::insert(complete.clone())
        .secure()
        .scope_with_model(scope, &complete)?;
    let mut active = stored.clone().into_active_model();
    let mut changed = false;
    for col in entity::date_policy::Column::iter() {
        if stored.get(col) != proposed.get(col) {
            active.set(col, proposed.get(col));
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    let result = entity::date_policy::Entity::update_many()
        .set(active)
        .filter(entity::date_policy::Column::PolicyId.eq(expected.policy_id))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await?;
    if result.rows_affected != 1 {
        return Err(ScopeError::Denied("conditional update lost"));
    }
    Ok(())
}
pub async fn replace_policy_election(
    tx: &impl TransactionRunner,
    scope: &AccessScope,
    expected: &entity::policy_election::Model,
    proposed: entity::policy_election::Model,
) -> Result<(), ScopeError> {
    if proposed.policy_id != expected.policy_id {
        return Err(ScopeError::Denied("immutable key"));
    }
    let stored = entity::policy_election::Entity::find_by_id(expected.policy_id)
        .lock_exclusive()
        .secure()
        .scope_with(scope)
        .one(tx)
        .await?
        .ok_or(ScopeError::Denied("hidden record"))?;
    if &stored != expected {
        return Err(ScopeError::Denied("stale storage facts or ownership fence"));
    }
    let complete = proposed.clone().into_active_model();
    entity::policy_election::Entity::insert(complete.clone())
        .secure()
        .scope_with_model(scope, &complete)?;
    let mut active = stored.clone().into_active_model();
    let mut changed = false;
    for col in entity::policy_election::Column::iter() {
        if stored.get(col) != proposed.get(col) {
            active.set(col, proposed.get(col));
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    let result = entity::policy_election::Entity::update_many()
        .set(active)
        .filter(entity::policy_election::Column::PolicyId.eq(expected.policy_id))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await?;
    if result.rows_affected != 1 {
        return Err(ScopeError::Denied("conditional update lost"));
    }
    Ok(())
}
/// Remove one resource-tenant override (the policy channel's retirement), fenced on the exact
/// stored row. The platform default is permanent (database guard); its namespace revision
/// advances in the same transaction, so a re-created override can never reuse a revision.
pub async fn delete_date_policy_override(
    tx: &impl TransactionRunner,
    scope: &AccessScope,
    expected: &entity::date_policy::Model,
) -> Result<(), ScopeError> {
    if expected.resource_tenant_id.is_none() {
        return Err(ScopeError::Denied("the platform default is permanent"));
    }
    let stored = entity::date_policy::Entity::find_by_id(expected.policy_id)
        .lock_exclusive()
        .secure()
        .scope_with(scope)
        .one(tx)
        .await?
        .ok_or(ScopeError::Denied("hidden record"))?;
    if &stored != expected {
        return Err(ScopeError::Denied("stale storage facts or ownership fence"));
    }
    let result = entity::date_policy::Entity::delete_many()
        .filter(entity::date_policy::Column::PolicyId.eq(expected.policy_id))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await?;
    if result.rows_affected != 1 {
        return Err(ScopeError::Denied("conditional delete lost"));
    }
    Ok(())
}
pub async fn replace_state_ttl_policy(
    tx: &impl TransactionRunner,
    scope: &AccessScope,
    expected: &entity::state_ttl_policy::Model,
    proposed: entity::state_ttl_policy::Model,
) -> Result<(), ScopeError> {
    if proposed.policy_id != expected.policy_id {
        return Err(ScopeError::Denied("immutable key"));
    }
    entity::state_ttl_policy::Entity::find()
        .filter(entity::state_ttl_policy::Column::Scope.eq("platform"))
        .filter(entity::state_ttl_policy::Column::State.eq(&expected.state))
        .lock_exclusive()
        .secure()
        .scope_with(scope)
        .one(tx)
        .await?
        .ok_or(ScopeError::Denied(
            "platform policy serialization scope required",
        ))?;
    let stored = entity::state_ttl_policy::Entity::find_by_id(expected.policy_id)
        .lock_exclusive()
        .secure()
        .scope_with(scope)
        .one(tx)
        .await?
        .ok_or(ScopeError::Denied("hidden record"))?;
    if &stored != expected {
        return Err(ScopeError::Denied("stale storage facts or ownership fence"));
    }
    let complete = proposed.clone().into_active_model();
    entity::state_ttl_policy::Entity::insert(complete.clone())
        .secure()
        .scope_with_model(scope, &complete)?;
    let mut active = stored.clone().into_active_model();
    let mut changed = false;
    for col in entity::state_ttl_policy::Column::iter() {
        if stored.get(col) != proposed.get(col) {
            active.set(col, proposed.get(col));
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    let result = entity::state_ttl_policy::Entity::update_many()
        .set(active)
        .filter(entity::state_ttl_policy::Column::PolicyId.eq(expected.policy_id))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await?;
    if result.rows_affected != 1 {
        return Err(ScopeError::Denied("conditional update lost"));
    }
    Ok(())
}
impl<T: TransactionRunner> LockedOrder<'_, T> {
    /// Removing membership never releases the reserved identity or deletes immutable history.
    pub(crate) async fn remove_draft_line(&self, line_id: Uuid) -> Result<bool, ScopeError> {
        let result = entity::draft_content::Entity::delete_many()
            .filter(entity::draft_content::Column::LineId.eq(line_id))
            .secure()
            .scope_with(&self.child_scope())
            .exec(self.tx)
            .await?;
        Ok(result.rows_affected == 1)
    }
    /// Exact newly inserted IDs only; mismatch aborts the caller's transaction.
    /// `released_at` is server database time (OL-2), never a caller or worker clock.
    pub(crate) async fn release_claims(&self, ids: &[Uuid]) -> Result<(), ScopeError> {
        if ids.is_empty() {
            return Ok(());
        }
        let unique: std::collections::BTreeSet<_> = ids.iter().copied().collect();
        if unique.len() != ids.len() {
            return Err(ScopeError::Invalid("duplicate release IDs"));
        }
        let result = entity::inflight_overlap_claim::Entity::update_many()
            .secure()
            .scope_with(&self.child_scope())
            .col_expr(
                entity::inflight_overlap_claim::Column::ReleasedAt,
                sea_orm::sea_query::Expr::cust("clock_timestamp()"),
            )
            .filter(
                Condition::all()
                    .add(entity::inflight_overlap_claim::Column::ClaimId.is_in(ids.iter().copied()))
                    .add(entity::inflight_overlap_claim::Column::ReleasedAt.is_null()),
            )
            .exec(self.tx)
            .await?;
        if result.rows_affected != ids.len() as u64 {
            return Err(ScopeError::Denied("claim release count mismatch"));
        }
        Ok(())
    }
}
