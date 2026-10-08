//! Current-parent reads and transaction-bound contribution writes.
use super::{
    AccessScope, DBRunner, EntityTrait, IntoActiveModel, LockedOrder, ScopeError, SecureEntityExt,
    SecureInsertExt, TransactionRunner, Uuid, order,
};
use crate::infra::storage::entity;
use sea_orm::{ColumnTrait, QueryFilter, QueryOrder};
pub async fn order_version_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::order_version::Model>, ScopeError> {
    entity::order_version::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::order_version::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn order_line_identity_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::order_line_identity::Model>, ScopeError> {
    entity::order_line_identity::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::order_line_identity::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn commercial_attempt_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::commercial_attempt::Model>, ScopeError> {
    entity::commercial_attempt::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::commercial_attempt::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn order_line_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::order_line::Model>, ScopeError> {
    entity::order_line::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::order_line::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn draft_content_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::draft_content::Model>, ScopeError> {
    entity::draft_content::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::draft_content::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn order_admin_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::order_admin::Model>, ScopeError> {
    entity::order_admin::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::order_admin::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn order_line_admin_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::order_line_admin::Model>, ScopeError> {
    entity::order_line_admin::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::order_line_admin::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn resolved_total_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::resolved_total::Model>, ScopeError> {
    entity::resolved_total::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::resolved_total::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn inflight_overlap_claim_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::inflight_overlap_claim::Model>, ScopeError> {
    entity::inflight_overlap_claim::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::inflight_overlap_claim::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn fulfillment_grant_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::fulfillment_grant::Model>, ScopeError> {
    entity::fulfillment_grant::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::fulfillment_grant::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn fulfillment_control_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::fulfillment_control::Model>, ScopeError> {
    entity::fulfillment_control::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::fulfillment_control::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn line_fulfillment_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::line_fulfillment::Model>, ScopeError> {
    entity::line_fulfillment::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::line_fulfillment::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn acceptance_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::acceptance::Model>, ScopeError> {
    entity::acceptance::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::acceptance::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn approval_reflection_for_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::approval_reflection::Model>, ScopeError> {
    entity::approval_reflection::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::approval_reflection::Column::OrderId.eq(id))
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
/// Sparse history is ordered by stored version, never generated by integer ranges.
pub async fn versions(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Vec<entity::order_version::Model>, ScopeError> {
    entity::order_version::Entity::find()
        .inner_join(order::Entity)
        .filter(entity::order_version::Column::OrderId.eq(id))
        .order_by_asc(entity::order_version::Column::Version)
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
pub async fn version(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
    version: i32,
) -> Result<Option<entity::order_version::Model>, ScopeError> {
    entity::order_version::Entity::find_by_id((id, version))
        .inner_join(order::Entity)
        .secure()
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<order::Entity>(scope)
        .one(runner)
        .await
}
impl<T: TransactionRunner> LockedOrder<'_, T> {
    pub(crate) async fn insert_order_version(
        &self,
        row: entity::order_version::Model,
    ) -> Result<entity::order_version::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::order_version::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_order_line_identity(
        &self,
        row: entity::order_line_identity::Model,
    ) -> Result<entity::order_line_identity::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::order_line_identity::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_commercial_attempt(
        &self,
        row: entity::commercial_attempt::Model,
    ) -> Result<entity::commercial_attempt::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::commercial_attempt::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_order_line(
        &self,
        row: entity::order_line::Model,
    ) -> Result<entity::order_line::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::order_line::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    /// Reserve a never-used line identity and add membership in this locked transaction.
    /// A removed identity stays reserved; its primary key rejects reuse before membership.
    pub async fn add_draft_line(
        &self,
        row: entity::draft_content::Model,
        created_at: time::OffsetDateTime,
    ) -> Result<entity::draft_content::Model, ScopeError> {
        self.insert_order_line_identity(entity::order_line_identity::Model {
            order_id: row.order_id,
            line_id: row.line_id,
            created_at,
        })
        .await?;
        let active = row.into_active_model();
        entity::draft_content::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_order_admin(
        &self,
        row: entity::order_admin::Model,
    ) -> Result<entity::order_admin::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::order_admin::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_order_line_admin(
        &self,
        row: entity::order_line_admin::Model,
    ) -> Result<entity::order_line_admin::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::order_line_admin::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_resolved_total(
        &self,
        row: entity::resolved_total::Model,
    ) -> Result<entity::resolved_total::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::resolved_total::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_fulfillment_grant(
        &self,
        row: entity::fulfillment_grant::Model,
    ) -> Result<entity::fulfillment_grant::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::fulfillment_grant::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_fulfillment_control(
        &self,
        row: entity::fulfillment_control::Model,
    ) -> Result<entity::fulfillment_control::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::fulfillment_control::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_line_fulfillment(
        &self,
        row: entity::line_fulfillment::Model,
    ) -> Result<entity::line_fulfillment::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::line_fulfillment::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_acceptance(
        &self,
        row: entity::acceptance::Model,
    ) -> Result<entity::acceptance::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::acceptance::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
    pub(crate) async fn insert_approval_reflection(
        &self,
        row: entity::approval_reflection::Model,
    ) -> Result<entity::approval_reflection::Model, ScopeError> {
        if row.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        let active = row.into_active_model();
        entity::approval_reflection::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .exec_with_returning(self.tx)
            .await
    }
}
