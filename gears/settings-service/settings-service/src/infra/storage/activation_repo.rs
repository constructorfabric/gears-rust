// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-schema:p1
//! `ActivationRepository` over `subscriptions` and `activation_state`.
//!
//! Both tables are written only here. The change's states go in by one
//! insert-select from `subscriptions` restricted to the read set's ids, so a
//! subscription retired since the read gets no row.

use async_trait::async_trait;
use sea_orm::sea_query::{Expr, Query, SelectStatement};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, EntityTrait, ExprTrait as _, QueryFilter, QueryOrder,
};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::recorder::Transition;
use crate::domain::activation::subscriptions::ActivationRepository;
use crate::domain::activation::{
    ActivationCounts, ActivationState, ActivationStatus, DueRow, NotifiedChange, SubscriberTenant,
    Subscription,
};
use crate::domain::error::DomainError;
use crate::infra::storage::clock::now;
use crate::infra::storage::entity::activation_state::{self, Entity as StateEntity};
use crate::infra::storage::entity::subscription::{self, Entity as SubscriptionEntity};

/// The repository. Stateless: every operation takes its connection.
#[derive(Debug, Default, Clone, Copy)]
pub struct ActivationRepo;

fn db_error(err: impl std::fmt::Display) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

fn subscription_of(model: subscription::Model) -> Subscription {
    Subscription {
        id: model.id,
        subscriber: model.subscriber,
        key: model.key,
        tenant: model.tenant,
        created_at: model.created_at,
    }
}

fn state_of(model: activation_state::Model) -> Result<ActivationState, DomainError> {
    let status = ActivationStatus::parse(&model.status).ok_or_else(|| DomainError::Internal {
        diagnostic: format!(
            "activation state of `{}` carries an unknown status `{}`",
            model.key, model.status
        ),
    })?;
    Ok(ActivationState {
        subscriber: model.subscriber,
        key: model.key,
        tenant: model.tenant,
        change_ref: model.change_ref,
        expected_value: model.expected_value,
        status,
        applied_value: model.applied_value,
        detail: model.detail,
        reported_at: model.reported_at,
        last_notified_at: model.last_notified_at,
        updated_at: model.updated_at,
    })
}

fn triple<E: EntityTrait>(
    subscriber: &str,
    key: &str,
    tenant: &str,
    columns: (E::Column, E::Column, E::Column),
) -> Condition
where
    E::Column: ColumnTrait,
{
    Condition::all()
        .add(columns.0.eq(subscriber))
        .add(columns.1.eq(key))
        .add(columns.2.eq(tenant))
}

/// The `expected_value` of each row: one literal when every row shares it,
/// else a `CASE` on the subscription's tenant.
fn expected_column(rows: &[StateRow]) -> sea_orm::sea_query::SimpleExpr {
    let shared = rows
        .first()
        .map(|r| r.expected_value.as_str())
        .filter(|first| rows.iter().all(|r| r.expected_value == *first));
    if let Some(shared) = shared {
        return Expr::val(shared.to_owned());
    }
    let mut arms = rows.iter();
    let Some(first) = arms.next() else {
        return Expr::val(String::new());
    };
    let tenant = || Expr::col((SubscriptionEntity, subscription::Column::Tenant));
    let mut case = Expr::case(
        tenant().eq(first.tenant.clone()),
        first.expected_value.clone(),
    );
    for row in arms {
        case = case.case(tenant().eq(row.tenant.clone()), row.expected_value.clone());
    }
    case.finally(Expr::val(String::new())).into()
}

/// The source of the insert-select: the read set's subscriptions that still
/// exist, as the `awaiting` states they become.
fn source(write: &StateWrite, at: OffsetDateTime) -> SelectStatement {
    let ids: Vec<Uuid> = write.rows.iter().map(|r| r.subscription_id).collect();
    Query::select()
        .column((SubscriptionEntity, subscription::Column::Subscriber))
        .column((SubscriptionEntity, subscription::Column::Key))
        .column((SubscriptionEntity, subscription::Column::Tenant))
        .expr(Expr::val(write.change_ref))
        .expr(expected_column(&write.rows))
        .expr(Expr::val(ActivationStatus::Awaiting.as_str()))
        .expr(Expr::val(at))
        .from(SubscriptionEntity)
        .and_where(Expr::col((SubscriptionEntity, subscription::Column::Id)).is_in(ids))
        .to_owned()
}

/// How many rows one stamp statement names: two bound values each, well under
/// every backend's bind limit.
const STAMP_CHUNK: usize = 400;

impl ActivationRepo {
    /// Every `awaiting` state of the pair, in key order, through
    /// `idx_activation_awaiting`.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    pub async fn awaiting_of<C: DBRunner>(
        &self,
        conn: &C,
        pair: &SubscriberTenant,
    ) -> Result<Vec<NotifiedChange>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-6
        let rows = StateEntity::find()
            .filter(activation_state::Column::Subscriber.eq(pair.subscriber.as_str()))
            .filter(activation_state::Column::Tenant.eq(pair.tenant.as_str()))
            .filter(activation_state::Column::Status.eq(ActivationStatus::Awaiting.as_str()))
            .order_by_asc(activation_state::Column::Key)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|row| NotifiedChange {
                key: row.key,
                change_ref: row.change_ref,
            })
            .collect())
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-6
    }

    /// Stamp `last_notified_at` on the pair's states in `changes` that still
    /// name their `change_ref` and are still `awaiting`: a row a later change
    /// overwrote meanwhile keeps the stamp absent. Returns how many rows were
    /// stamped.
    ///
    /// # Errors
    /// [`DomainError`] when a statement fails.
    pub async fn stamp<C: DBRunner>(
        &self,
        conn: &C,
        pair: &SubscriberTenant,
        changes: &[NotifiedChange],
        at: OffsetDateTime,
    ) -> Result<u64, DomainError> {
        let scope = AccessScope::allow_all();
        let mut stamped = 0;
        for chunk in changes.chunks(STAMP_CHUNK) {
            let mut named = Condition::any();
            for change in chunk {
                named = named.add(
                    Condition::all()
                        .add(activation_state::Column::Key.eq(change.key.as_str()))
                        .add(activation_state::Column::ChangeRef.eq(change.change_ref)),
                );
            }
            stamped += StateEntity::update_many()
                .col_expr(activation_state::Column::LastNotifiedAt, Expr::value(at))
                .filter(activation_state::Column::Subscriber.eq(pair.subscriber.as_str()))
                .filter(activation_state::Column::Tenant.eq(pair.tenant.as_str()))
                .filter(activation_state::Column::Status.eq(ActivationStatus::Awaiting.as_str()))
                .filter(named)
                .secure()
                .scope_with(&scope)
                .exec(conn)
                .await
                .map_err(db_error)?
                .rows_affected;
        }
        Ok(stamped)
    }

    /// The `awaiting` states never published or last published before
    /// `cutoff`, ordered by subscriber, tenant and key, through
    /// `idx_activation_awaiting`.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    pub async fn due<C: DBRunner>(
        &self,
        conn: &C,
        cutoff: OffsetDateTime,
    ) -> Result<Vec<DueRow>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-5
        let rows = StateEntity::find()
            .filter(activation_state::Column::Status.eq(ActivationStatus::Awaiting.as_str()))
            .filter(
                Condition::any()
                    .add(activation_state::Column::LastNotifiedAt.is_null())
                    .add(activation_state::Column::LastNotifiedAt.lt(cutoff)),
            )
            .order_by_asc(activation_state::Column::Subscriber)
            .order_by_asc(activation_state::Column::Tenant)
            .order_by_asc(activation_state::Column::Key)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|row| DueRow {
                pair: SubscriberTenant {
                    subscriber: row.subscriber,
                    tenant: row.tenant,
                },
                change: NotifiedChange {
                    key: row.key,
                    change_ref: row.change_ref,
                },
            })
            .collect())
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-5
    }

    /// Delete, in one conditional statement, every state whose subscription no
    /// longer exists, whatever its status; a state whose subscription exists is
    /// untouched. Returns how many were deleted.
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    pub async fn sweep_orphans<C: DBRunner>(&self, conn: &C) -> Result<u64, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-4
        let subscribed = Query::select()
            .expr(Expr::val(1))
            .from(SubscriptionEntity)
            .and_where(
                Expr::col((SubscriptionEntity, subscription::Column::Subscriber))
                    .equals((StateEntity, activation_state::Column::Subscriber)),
            )
            .and_where(
                Expr::col((SubscriptionEntity, subscription::Column::Key))
                    .equals((StateEntity, activation_state::Column::Key)),
            )
            .and_where(
                Expr::col((SubscriptionEntity, subscription::Column::Tenant))
                    .equals((StateEntity, activation_state::Column::Tenant)),
            )
            .to_owned();
        Ok(StateEntity::delete_many()
            .filter(Expr::exists(subscribed).not())
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_error)?
            .rows_affected)
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-4
    }

    /// The subscriptions and the `awaiting` states, for the gauges.
    ///
    /// # Errors
    /// [`DomainError`] when a count fails.
    pub async fn counts<C: DBRunner>(&self, conn: &C) -> Result<ActivationCounts, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-8
        let scope = AccessScope::allow_all();
        let subscriptions = SubscriptionEntity::find()
            .secure()
            .scope_with(&scope)
            .count(conn)
            .await
            .map_err(db_error)?;
        let awaiting = StateEntity::find()
            .filter(activation_state::Column::Status.eq(ActivationStatus::Awaiting.as_str()))
            .secure()
            .scope_with(&scope)
            .count(conn)
            .await
            .map_err(db_error)?;
        Ok(ActivationCounts {
            subscriptions,
            awaiting,
        })
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-8
    }
}

#[async_trait]
impl ActivationRepository for ActivationRepo {
    async fn register<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Subscription, DomainError> {
        if let Some(existing) = self.find(conn, subscriber, key, tenant).await? {
            return Ok(existing);
        }
        let active = subscription::ActiveModel {
            id: Set(Uuid::now_v7()),
            subscriber: Set(subscriber.to_owned()),
            key: Set(key.to_owned()),
            tenant: Set(tenant.to_owned()),
            created_at: Set(now()),
        };
        // The pre-`find` is the idempotency: a registration runs under the
        // key's mutation lock, so no other registration of the triple lands
        // between it and this insert, and a unique violation is not expected —
        // were one to occur, it is the statement's failure, since on
        // `PostgreSQL` it would abort the transaction and no follow-up read
        // could run.
        toolkit_db::secure::secure_insert::<SubscriptionEntity>(
            active,
            &AccessScope::allow_all(),
            conn,
        )
        .await
        .map(subscription_of)
        .map_err(db_error)
    }

    async fn of_key<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
    ) -> Result<Vec<Subscription>, DomainError> {
        let rows = SubscriptionEntity::find()
            .filter(subscription::Column::Key.eq(key))
            .order_by_asc(subscription::Column::CreatedAt)
            .order_by_asc(subscription::Column::Id)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(subscription_of).collect())
    }

    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Option<Subscription>, DomainError> {
        let row = SubscriptionEntity::find()
            .filter(triple::<SubscriptionEntity>(
                subscriber,
                key,
                tenant,
                (
                    subscription::Column::Subscriber,
                    subscription::Column::Key,
                    subscription::Column::Tenant,
                ),
            ))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(conn)
            .await
            .map_err(db_error)?;
        Ok(row.map(subscription_of))
    }

    async fn retire<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<bool, DomainError> {
        let scope = AccessScope::allow_all();
        let existed = SubscriptionEntity::delete_many()
            .filter(triple::<SubscriptionEntity>(
                subscriber,
                key,
                tenant,
                (
                    subscription::Column::Subscriber,
                    subscription::Column::Key,
                    subscription::Column::Tenant,
                ),
            ))
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?
            .rows_affected
            > 0;
        // The state goes with the subscription, in the same transaction;
        // nothing is reported for it.
        StateEntity::delete_many()
            .filter(triple::<StateEntity>(
                subscriber,
                key,
                tenant,
                (
                    activation_state::Column::Subscriber,
                    activation_state::Column::Key,
                    activation_state::Column::Tenant,
                ),
            ))
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok(existed)
    }

    async fn write_states<C: DBRunner>(
        &self,
        conn: &C,
        write: &StateWrite,
    ) -> Result<Vec<SubscriberTenant>, DomainError> {
        if write.rows.is_empty() {
            return Ok(Vec::new());
        }
        let scope = AccessScope::allow_all();
        let ids: Vec<Uuid> = write.rows.iter().map(|r| r.subscription_id).collect();
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-20
        // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-1
        // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-4
        // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-5
        // The state of an affected subscription is replaced whatever it was: its
        // row is cleared and the insert-select writes it afresh. A report that
        // meets the cleared row finds the expectation it answered gone and is
        // ignored, as a report on a replaced `change_ref` is. Only the rows of
        // subscriptions that still exist are cleared — the ones the insert
        // below writes anew.
        let still_subscribed = Query::select()
            .expr(Expr::val(1))
            .from(SubscriptionEntity)
            .and_where(Expr::col((SubscriptionEntity, subscription::Column::Id)).is_in(ids))
            .and_where(
                Expr::col((SubscriptionEntity, subscription::Column::Subscriber))
                    .equals((StateEntity, activation_state::Column::Subscriber)),
            )
            .and_where(
                Expr::col((SubscriptionEntity, subscription::Column::Key))
                    .equals((StateEntity, activation_state::Column::Key)),
            )
            .and_where(
                Expr::col((SubscriptionEntity, subscription::Column::Tenant))
                    .equals((StateEntity, activation_state::Column::Tenant)),
            )
            .to_owned();
        StateEntity::delete_many()
            .filter(Expr::exists(still_subscribed))
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-21
        // A subscription of the read set retired before this statement is not in
        // `subscriptions` for the select to find, and gets no row.
        toolkit_db::secure::secure_insert_from_select::<StateEntity, _>(
            [
                activation_state::Column::Subscriber,
                activation_state::Column::Key,
                activation_state::Column::Tenant,
                activation_state::Column::ChangeRef,
                activation_state::Column::ExpectedValue,
                activation_state::Column::Status,
                activation_state::Column::UpdatedAt,
            ],
            source(write, now()),
            &scope,
            conn,
        )
        .await
        .map_err(db_error)?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-21
        // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-5
        // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-4
        // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-1
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-20
        let written = StateEntity::find()
            .filter(activation_state::Column::Key.eq(write.key.as_str()))
            .filter(activation_state::Column::ChangeRef.eq(write.change_ref))
            .order_by_asc(activation_state::Column::Subscriber)
            .order_by_asc(activation_state::Column::Tenant)
            .secure()
            .scope_with(&scope)
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(written
            .into_iter()
            .map(|row| SubscriberTenant {
                subscriber: row.subscriber,
                tenant: row.tenant,
            })
            .collect())
    }

    async fn state<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Option<ActivationState>, DomainError> {
        let row = StateEntity::find()
            .filter(triple::<StateEntity>(
                subscriber,
                key,
                tenant,
                (
                    activation_state::Column::Subscriber,
                    activation_state::Column::Key,
                    activation_state::Column::Tenant,
                ),
            ))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(conn)
            .await
            .map_err(db_error)?;
        row.map(state_of).transpose()
    }

    async fn states_of<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
        tenant: &str,
    ) -> Result<Vec<ActivationState>, DomainError> {
        // One statement on the key and the tenant, through
        // `idx_activation_key_tenant`, in subscriber order; a failed read is
        // the store being unavailable, never an absent state.
        let rows = StateEntity::find()
            .filter(activation_state::Column::Key.eq(key))
            .filter(activation_state::Column::Tenant.eq(tenant))
            .order_by_asc(activation_state::Column::Subscriber)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(|err| {
                DomainError::dependency_unavailable("activation store", "read the states", err)
            })?;
        rows.into_iter().map(state_of).collect()
    }

    async fn report<C: DBRunner>(
        &self,
        conn: &C,
        transition: &Transition,
    ) -> Result<Option<OffsetDateTime>, DomainError> {
        let at = now();
        let updated = StateEntity::update_many()
            .col_expr(
                activation_state::Column::Status,
                Expr::value(transition.status.as_str()),
            )
            .col_expr(
                activation_state::Column::AppliedValue,
                Expr::value(transition.applied_value.clone()),
            )
            .col_expr(
                activation_state::Column::Detail,
                Expr::value(transition.detail.clone()),
            )
            .col_expr(activation_state::Column::ReportedAt, Expr::value(at))
            .col_expr(activation_state::Column::UpdatedAt, Expr::value(at))
            .filter(triple::<StateEntity>(
                &transition.subscriber,
                &transition.key,
                &transition.tenant,
                (
                    activation_state::Column::Subscriber,
                    activation_state::Column::Key,
                    activation_state::Column::Tenant,
                ),
            ))
            // The row guard: the `change_ref` the decision read, still awaiting.
            .filter(activation_state::Column::ChangeRef.eq(transition.change_ref))
            .filter(activation_state::Column::Status.eq(ActivationStatus::Awaiting.as_str()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok((updated.rows_affected > 0).then_some(at))
    }
}

#[cfg(test)]
#[path = "activation_repo_tests.rs"]
mod activation_repo_tests;
