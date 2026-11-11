// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-schema:p1
//! `ActivationRepository` over `subscriptions` and `activation_state`, one
//! row of each per consumer instance: `(subscriber, instance_id, key, tenant)`.
//!
//! Both tables are written only here. The change's states go in by one
//! insert-select from `subscriptions` restricted to the read set's ids, so a
//! subscription unsubscribed since the read gets no row, and which silent
//! instances the change leaves out is a predicate of that one statement. Every
//! statement is plain: the decisions, and the order in which a writer runs
//! them — an instance's state before its subscription — are the callers'. The
//! one order kept here is the order of row locks: a statement that may change
//! several states locks them by `(subscriber, instance_id, key, tenant)`
//! whatever its plan ([`locked_in_order`]).

use async_trait::async_trait;
use sea_orm::sea_query::{Alias, Expr, LockType, Order, Query, SelectStatement};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, EntityTrait, ExprTrait as _, QueryFilter, QueryOrder,
    QuerySelect as _,
};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::recorder::Transition;
use crate::domain::activation::subscriptions::ActivationRepository;
use crate::domain::activation::{
    ActivationCounts, ActivationRow, ActivationState, ActivationStatus, DueRow, InstanceKey,
    NotifiedChange, SubscriberTenant, Subscription,
};
use crate::domain::error::DomainError;
use crate::infra::storage::clock::now;
use crate::infra::storage::entity::activation_state::{self, Entity as StateEntity};
use crate::infra::storage::entity::subscription::{self, Entity as SubscriptionEntity};

/// The columns the activation read selects: no value, no hash, no delivery
/// stamp.
#[derive(Debug, sea_orm::FromQueryResult)]
struct ActivationRowModel {
    subscriber: String,
    instance_id: Uuid,
    change_ref: Uuid,
    status: String,
    silent_since: Option<OffsetDateTime>,
    reported_at: Option<OffsetDateTime>,
    detail: Option<String>,
    instance_label: Option<String>,
}

/// The repository. Stateless: every operation takes its connection.
#[derive(Debug, Default, Clone, Copy)]
pub struct ActivationRepo;

fn db_error(err: impl std::fmt::Display) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

fn count_of(rows: u64) -> Result<usize, DomainError> {
    usize::try_from(rows).map_err(db_error)
}

fn subscription_of(model: subscription::Model) -> Subscription {
    Subscription {
        id: model.id,
        subscriber: model.subscriber,
        instance_id: model.instance_id,
        instance_label: model.instance_label,
        key: model.key,
        tenant: model.tenant,
        silent_since: model.silent_since,
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
    let deliveries = u32::try_from(model.deliveries).map_err(|_| DomainError::Internal {
        diagnostic: format!(
            "activation state of `{}` carries a negative delivery count `{}`",
            model.key, model.deliveries
        ),
    })?;
    Ok(ActivationState {
        subscriber: model.subscriber,
        instance_id: model.instance_id,
        key: model.key,
        tenant: model.tenant,
        change_ref: model.change_ref,
        changed_at: model.changed_at,
        expected_value: model.expected_value,
        status,
        deliveries,
        silent_since: model.silent_since,
        applied_value: model.applied_value,
        detail: model.detail,
        reported_at: model.reported_at,
        last_notified_at: model.last_notified_at,
        updated_at: model.updated_at,
    })
}

/// The condition naming one instance's subscription.
fn subscription_is(instance: &InstanceKey) -> Condition {
    Condition::all()
        .add(subscription::Column::Subscriber.eq(instance.subscriber.as_str()))
        .add(subscription::Column::InstanceId.eq(instance.instance_id))
        .add(subscription::Column::Key.eq(instance.key.as_str()))
        .add(subscription::Column::Tenant.eq(instance.tenant.as_str()))
}

/// The condition naming one instance's activation state.
fn state_is(instance: &InstanceKey) -> Condition {
    Condition::all()
        .add(activation_state::Column::Subscriber.eq(instance.subscriber.as_str()))
        .add(activation_state::Column::InstanceId.eq(instance.instance_id))
        .add(activation_state::Column::Key.eq(instance.key.as_str()))
        .add(activation_state::Column::Tenant.eq(instance.tenant.as_str()))
}

/// The registrations of one subscriber for one key at one tenant.
fn registrations_of(subscriber: &str, key: &str, tenant: &str) -> Condition {
    Condition::all()
        .add(subscription::Column::Subscriber.eq(subscriber))
        .add(subscription::Column::Key.eq(key))
        .add(subscription::Column::Tenant.eq(tenant))
}

/// `subscriptions` and `activation_state` rows of one instance, correlated:
/// the four identity columns of the state equal the subscription's, whichever
/// of the two tables is the outer one.
fn same_instance(select: &mut SelectStatement) {
    for (sub, state) in [
        (
            subscription::Column::Subscriber,
            activation_state::Column::Subscriber,
        ),
        (
            subscription::Column::InstanceId,
            activation_state::Column::InstanceId,
        ),
        (subscription::Column::Key, activation_state::Column::Key),
        (
            subscription::Column::Tenant,
            activation_state::Column::Tenant,
        ),
    ] {
        select.and_where(Expr::col((SubscriptionEntity, sub)).equals((StateEntity, state)));
    }
}

/// The columns of `uq_activation_state`, in the canonical order in which every
/// multi-row statement over `activation_state` takes its row locks.
const LOCK_ORDER: [activation_state::Column; 4] = [
    activation_state::Column::Subscriber,
    activation_state::Column::InstanceId,
    activation_state::Column::Key,
    activation_state::Column::Tenant,
];

/// The rows of `activation_state` that `condition` names, locked first and in
/// the canonical order: `(subscriber, instance_id, key, tenant) IN (SELECT …
/// FROM activation_state WHERE condition ORDER BY subscriber, instance_id, key,
/// tenant FOR <lock>)`, the filter of a multi-row UPDATE or DELETE.
///
/// The locks are what a plain multi-row statement leaves to its plan: it locks
/// each row as its scan reaches it, in the order of whichever index or join the
/// planner picked, and two such statements meeting on rows of one pair may lock
/// them in opposite orders and deadlock, a third transaction holding one row in
/// between being enough. Here the sub-select's lock step runs above its sort, so
/// it locks the rows in the canonical order whatever the plan, and the outer
/// statement reaches only rows the sub-select has returned — already locked by
/// it. Every multi-row statement over the table locks in this one order, so no
/// two of them wait on each other in a cycle. The sub-select reads the rows of
/// its own `FROM`: a condition correlated on `activation_state` names them.
/// `SQLite` renders no lock clause and serializes writers itself.
fn locked_in_order(
    condition: impl sea_orm::sea_query::IntoCondition,
    lock: LockType,
) -> sea_orm::sea_query::SimpleExpr {
    let mut locking = Query::select();
    locking
        .columns(LOCK_ORDER.map(|column| (StateEntity, column)))
        .from(StateEntity)
        .cond_where(condition);
    for column in LOCK_ORDER {
        locking.order_by((StateEntity, column), Order::Asc);
    }
    locking.lock(lock);
    Expr::tuple(LOCK_ORDER.map(|column| Expr::col((StateEntity, column)))).in_subquery(locking)
}

/// The `expected_value` of each row: one literal when every row shares it,
/// else a `CASE` on the subscription's tenant — every instance at one tenant
/// shares the expected value there.
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
/// exist and that the change expects, as the `awaiting` states they become.
///
/// A subscription is expected when its `silent_since` is absent, or when no
/// subscription of the read set at the same subscriber, key and tenant has an
/// absent one — every instance of the subscriber there is silent, so all of
/// them are expected again. Both are read by this one statement, at its
/// snapshot.
fn source(write: &StateWrite, ids: &[Uuid], at: OffsetDateTime) -> SelectStatement {
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-30
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-31
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-32
    let sibling = Alias::new("sibling");
    let mut expected_sibling = Query::select();
    expected_sibling
        .expr(Expr::val(1))
        .from_as(SubscriptionEntity, sibling.clone())
        .and_where(Expr::col((sibling.clone(), subscription::Column::Id)).is_in(ids.to_vec()))
        .and_where(Expr::col((sibling.clone(), subscription::Column::SilentSince)).is_null());
    for column in [
        subscription::Column::Subscriber,
        subscription::Column::Key,
        subscription::Column::Tenant,
    ] {
        expected_sibling
            .and_where(Expr::col((sibling.clone(), column)).equals((SubscriptionEntity, column)));
    }
    Query::select()
        .column((SubscriptionEntity, subscription::Column::Subscriber))
        .column((SubscriptionEntity, subscription::Column::InstanceId))
        .column((SubscriptionEntity, subscription::Column::Key))
        .column((SubscriptionEntity, subscription::Column::Tenant))
        .expr(Expr::val(write.change_ref))
        // The database's clock, not this process's: the latency baseline.
        .expr(Expr::current_timestamp())
        .expr(expected_column(&write.rows))
        .expr(Expr::val(ActivationStatus::Awaiting.as_str()))
        .expr(Expr::val(0))
        .expr(Expr::val(at))
        .from(SubscriptionEntity)
        .and_where(Expr::col((SubscriptionEntity, subscription::Column::Id)).is_in(ids.to_vec()))
        .cond_where(
            Condition::any()
                .add(Expr::col((SubscriptionEntity, subscription::Column::SilentSince)).is_null())
                .add(Expr::exists(expected_sibling).not()),
        )
        .to_owned()
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-32
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-31
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-30
}

/// How many rows one per-instance stamp statement names: six bound values
/// each, well under every backend's bind limit.
const DUE_STAMP_CHUNK: usize = 200;

/// The `awaiting` states never published or last published at or before
/// `cutoff` — `now` less the interval, so a stamp exactly one interval old is
/// due: the due ones, through `idx_activation_awaiting`. A `silent` state is
/// never due.
fn due_states(cutoff: OffsetDateTime) -> sea_orm::Select<StateEntity> {
    StateEntity::find()
        .filter(activation_state::Column::Status.eq(ActivationStatus::Awaiting.as_str()))
        .filter(
            Condition::any()
                .add(activation_state::Column::LastNotifiedAt.is_null())
                .add(activation_state::Column::LastNotifiedAt.lte(cutoff)),
        )
}

fn due_row(model: activation_state::Model) -> Result<DueRow, DomainError> {
    let state = state_of(model)?;
    Ok(DueRow {
        pair: SubscriberTenant {
            subscriber: state.subscriber,
            tenant: state.tenant,
        },
        instance_id: state.instance_id,
        change: NotifiedChange {
            key: state.key,
            change_ref: state.change_ref,
        },
        deliveries: state.deliveries,
    })
}

impl ActivationRepo {
    /// The pair's due states — `awaiting`, never published or last published
    /// at or before `cutoff` — of every instance of the subscriber, in key and
    /// instance order, through `idx_activation_awaiting`: one entry per
    /// state, so a key several instances await comes once per instance.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    pub async fn due_of<C: DBRunner>(
        &self,
        conn: &C,
        pair: &SubscriberTenant,
        cutoff: OffsetDateTime,
    ) -> Result<Vec<DueRow>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-6
        let rows = due_states(cutoff)
            .filter(activation_state::Column::Subscriber.eq(pair.subscriber.as_str()))
            .filter(activation_state::Column::Tenant.eq(pair.tenant.as_str()))
            .order_by_asc(activation_state::Column::Key)
            .order_by_asc(activation_state::Column::InstanceId)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_error)?;
        rows.into_iter().map(due_row).collect()
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-6
    }

    /// Stamp a publication the broker accepted on exactly the due states it
    /// bundled: `last_notified_at` = `at` and `deliveries` incremented, on each
    /// row of `rows` still naming its `change_ref`, still `awaiting` and still
    /// at the `deliveries` it was read due with; a row a later change
    /// overwrote, a report settled or a call of silence made `silent`
    /// meanwhile is left alone, and so is every row not in `rows`. The count is
    /// the guard that the row is still as it was read due: another publisher
    /// that read it too — the end of a request and a tick, or two replicas —
    /// and stamped it first moved the count on, so its publication is counted
    /// once, never twice, whatever the replicas' clocks. Returns how many rows
    /// were stamped.
    ///
    /// The statement takes its row locks in the canonical order
    /// ([`locked_in_order`]), the guard both selecting the rows it locks and,
    /// rechecked on each row's newest version, the rows it stamps. Each chunk
    /// is a statement of its own; on a connection outside a transaction, as
    /// the store calls it, no lock outlives its chunk.
    ///
    /// # Errors
    /// [`DomainError`] when a statement fails.
    pub async fn stamp_due<C: DBRunner>(
        &self,
        conn: &C,
        rows: &[DueRow],
        at: OffsetDateTime,
    ) -> Result<u64, DomainError> {
        let scope = AccessScope::allow_all();
        let mut stamped = 0;
        for chunk in rows.chunks(DUE_STAMP_CHUNK) {
            let mut named = Condition::any();
            for row in chunk {
                named = named.add(
                    state_is(&row.instance_key())
                        .add(activation_state::Column::ChangeRef.eq(row.change.change_ref))
                        // Read from the column, so it fits; a count that did
                        // not would match no row and count nothing.
                        .add(
                            activation_state::Column::Deliveries
                                .eq(i32::try_from(row.deliveries).unwrap_or(i32::MAX)),
                        ),
                );
            }
            let guard = Condition::all()
                .add(activation_state::Column::Status.eq(ActivationStatus::Awaiting.as_str()))
                .add(named);
            stamped += StateEntity::update_many()
                .col_expr(activation_state::Column::LastNotifiedAt, Expr::value(at))
                .col_expr(
                    activation_state::Column::Deliveries,
                    Expr::col(activation_state::Column::Deliveries).add(1),
                )
                .filter(guard.clone())
                .filter(locked_in_order(guard, LockType::NoKeyUpdate))
                .secure()
                .scope_with(&scope)
                .exec(conn)
                .await
                .map_err(db_error)?
                .rows_affected;
        }
        Ok(stamped)
    }

    /// The `awaiting` states never published or last published at or before
    /// `cutoff`, with their instance and delivery count, ordered by
    /// subscriber, tenant, key and instance, through `idx_activation_awaiting`.
    /// A `silent` state is never due.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    pub async fn due<C: DBRunner>(
        &self,
        conn: &C,
        cutoff: OffsetDateTime,
    ) -> Result<Vec<DueRow>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-5
        let rows = due_states(cutoff)
            .order_by_asc(activation_state::Column::Subscriber)
            .order_by_asc(activation_state::Column::Tenant)
            .order_by_asc(activation_state::Column::Key)
            .order_by_asc(activation_state::Column::InstanceId)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_error)?;
        rows.into_iter().map(due_row).collect()
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-5
    }

    /// Call the instance's state of `change_ref` silent: `status` `silent`,
    /// `silent_since` and `updated_at` = `at`, guarded on the row still naming
    /// `change_ref` and still being `awaiting`. `true` when the guard matched —
    /// only then may the caller mark the subscription and write the record.
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    pub async fn call_silent<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
        change_ref: Uuid,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        #[cfg(any(test, feature = "integration"))]
        crate::infra::storage::activation_hold::reached(
            crate::infra::storage::activation_hold::Step::CallSilent,
            &instance.key,
        )
        .await;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-22
        // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-7
        let updated = StateEntity::update_many()
            .col_expr(
                activation_state::Column::Status,
                Expr::value(ActivationStatus::Silent.as_str()),
            )
            .col_expr(activation_state::Column::SilentSince, Expr::value(at))
            .col_expr(activation_state::Column::UpdatedAt, Expr::value(at))
            .filter(state_is(instance))
            .filter(activation_state::Column::ChangeRef.eq(change_ref))
            .filter(activation_state::Column::Status.eq(ActivationStatus::Awaiting.as_str()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok(updated.rows_affected > 0)
        // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-7
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-22
    }

    /// Set `silent_since` = `at` on the instance's subscription unless one is
    /// already set, so the forgetting day counts from the first call of
    /// silence. `true` when a subscription was marked.
    ///
    /// Either way the subscription's row is held until the call commits: a
    /// subscription already silent is locked `FOR UPDATE` though nothing is
    /// written to it. A report's or a registration's clear of `silent_since`
    /// takes that row first ([`ActivationRepository::clear_silent`]), so it
    /// waits for the call's commit and then sees the `silent` state the call
    /// made, rather than reading the state as it was before the call and
    /// clearing a silence the call is about to commit.
    ///
    /// # Errors
    /// [`DomainError`] when a statement fails.
    pub async fn mark_silent<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-24
        let scope = AccessScope::allow_all();
        let updated = SubscriptionEntity::update_many()
            .col_expr(subscription::Column::SilentSince, Expr::value(at))
            .filter(subscription_is(instance))
            .filter(subscription::Column::SilentSince.is_null())
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        if updated.rows_affected > 0 {
            return Ok(true);
        }
        // Already silent: held all the same. `SQLite` renders no lock clause
        // and serializes writers itself.
        SubscriptionEntity::find()
            .filter(subscription_is(instance))
            .lock_exclusive()
            .secure()
            .scope_with(&scope)
            .one(conn)
            .await
            .map_err(db_error)?;
        Ok(false)
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-24
    }

    /// The subscriptions whose `silent_since` is before `cutoff`, through
    /// `idx_sub_silent`, oldest silence first: the ones the forgetting
    /// deletes.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    pub async fn forgettable<C: DBRunner>(
        &self,
        conn: &C,
        cutoff: OffsetDateTime,
    ) -> Result<Vec<Subscription>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-17
        let rows = SubscriptionEntity::find()
            .filter(subscription::Column::SilentSince.is_not_null())
            .filter(subscription::Column::SilentSince.lt(cutoff))
            .order_by_asc(subscription::Column::SilentSince)
            .order_by_asc(subscription::Column::Id)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(subscription_of).collect())
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-17
    }

    /// Forget one silent instance: delete its activation state, then its
    /// subscription, each statement conditional on the subscription's
    /// `silent_since` still being before `cutoff` — a subscription a report or
    /// a registration cleared meanwhile keeps its state and itself. `true`
    /// when the subscription was deleted.
    ///
    /// # Errors
    /// [`DomainError`] when a statement fails.
    pub async fn forget<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
        cutoff: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        #[cfg(any(test, feature = "integration"))]
        crate::infra::storage::activation_hold::reached(
            crate::infra::storage::activation_hold::Step::Forget,
            &instance.key,
        )
        .await;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-18
        let scope = AccessScope::allow_all();
        let still_silent = || {
            Condition::all()
                .add(subscription_is(instance))
                .add(subscription::Column::SilentSince.is_not_null())
                .add(subscription::Column::SilentSince.lt(cutoff))
        };
        let silent_subscription = Query::select()
            .expr(Expr::val(1))
            .from(SubscriptionEntity)
            .cond_where(still_silent())
            .to_owned();
        StateEntity::delete_many()
            .filter(state_is(instance))
            .filter(Expr::exists(silent_subscription))
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        #[cfg(any(test, feature = "integration"))]
        crate::infra::storage::activation_hold::reached(
            crate::infra::storage::activation_hold::Step::ForgetSubscription,
            &instance.key,
        )
        .await;
        let deleted = SubscriptionEntity::delete_many()
            .filter(still_silent())
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok(deleted.rows_affected > 0)
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-18
    }

    /// Delete, in one conditional statement, every state whose subscription no
    /// longer exists, whatever its status; a state whose subscription exists is
    /// untouched. Returns how many were deleted. The rows are locked in the
    /// canonical order ([`locked_in_order`]).
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    pub async fn sweep_orphans<C: DBRunner>(&self, conn: &C) -> Result<u64, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-4
        let mut subscribed = Query::select();
        subscribed.expr(Expr::val(1)).from(SubscriptionEntity);
        same_instance(&mut subscribed);
        Ok(StateEntity::delete_many()
            .filter(locked_in_order(
                Expr::exists(subscribed).not(),
                LockType::Update,
            ))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_error)?
            .rows_affected)
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-4
    }

    /// The subscriptions, the `awaiting` and the `silent` states, and the
    /// subscriptions with `silent_since` set, for the gauges.
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
        let silent_subscriptions = SubscriptionEntity::find()
            .filter(subscription::Column::SilentSince.is_not_null())
            .secure()
            .scope_with(&scope)
            .count(conn)
            .await
            .map_err(db_error)?;
        let mut of_status = [0; 2];
        for (count, status) in of_status
            .iter_mut()
            .zip([ActivationStatus::Awaiting, ActivationStatus::Silent])
        {
            *count = StateEntity::find()
                .filter(activation_state::Column::Status.eq(status.as_str()))
                .secure()
                .scope_with(&scope)
                .count(conn)
                .await
                .map_err(db_error)?;
        }
        let [awaiting, silent] = of_status;
        Ok(ActivationCounts {
            subscriptions,
            awaiting,
            silent,
            silent_subscriptions,
        })
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-8
    }
}

#[async_trait]
impl ActivationRepository for ActivationRepo {
    async fn register<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
        instance_label: Option<&str>,
    ) -> Result<Subscription, DomainError> {
        #[cfg(any(test, feature = "integration"))]
        crate::infra::storage::activation_hold::reached(
            crate::infra::storage::activation_hold::Step::Register,
            &instance.key,
        )
        .await;
        let scope = AccessScope::allow_all();
        let label = instance_label.map(str::to_owned);
        if let Some(existing) = self.find(conn, instance).await? {
            #[cfg(any(test, feature = "integration"))]
            crate::infra::storage::activation_hold::reached(
                crate::infra::storage::activation_hold::Step::RegisterRefresh,
                &instance.key,
            )
            .await;
            // A repeat registration of the instance keeps its row and
            // refreshes its label; `silent_since` is not touched here.
            let refreshed = SubscriptionEntity::update_many()
                .col_expr(
                    subscription::Column::InstanceLabel,
                    Expr::value(label.clone()),
                )
                .filter(subscription::Column::Id.eq(existing.id))
                .secure()
                .scope_with(&scope)
                .exec(conn)
                .await
                .map_err(db_error)?
                .rows_affected;
            if refreshed > 0 {
                return Ok(Subscription {
                    instance_label: label,
                    ..existing
                });
            }
            // The forgetting, which takes no key lock, deleted the row after
            // the lookup and committed: the refresh, a new statement, saw the
            // delete. The registration inserts a new subscription below, as
            // after a forgetting that came first; the deleted row is
            // committed, so the insert meets no unique violation.
        }
        let active = subscription::ActiveModel {
            id: Set(Uuid::now_v7()),
            subscriber: Set(instance.subscriber.clone()),
            instance_id: Set(instance.instance_id),
            instance_label: Set(label),
            key: Set(instance.key.clone()),
            tenant: Set(instance.tenant.clone()),
            silent_since: Set(None),
            created_at: Set(now()),
        };
        // The pre-`find` is the idempotency: a registration runs under the
        // key's mutation lock, so no other registration of the instance lands
        // between it and this insert, and a unique violation is not expected —
        // were one to occur, it is the statement's failure, since on
        // `PostgreSQL` it would abort the transaction and no follow-up read
        // could run.
        toolkit_db::secure::secure_insert::<SubscriptionEntity>(active, &scope, conn)
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
        instance: &InstanceKey,
    ) -> Result<Option<Subscription>, DomainError> {
        let row = SubscriptionEntity::find()
            .filter(subscription_is(instance))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(conn)
            .await
            .map_err(db_error)?;
        Ok(row.map(subscription_of))
    }

    async fn registrations<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<u64, DomainError> {
        SubscriptionEntity::find()
            .filter(registrations_of(subscriber, key, tenant))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .count(conn)
            .await
            .map_err(db_error)
    }

    async fn oldest_evictable<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Option<Subscription>, DomainError> {
        let mut holds_state = Query::select();
        holds_state.expr(Expr::val(1)).from(StateEntity);
        same_instance(&mut holds_state);
        let row = SubscriptionEntity::find()
            .filter(registrations_of(subscriber, key, tenant))
            .filter(
                Condition::any()
                    .add(subscription::Column::SilentSince.is_not_null())
                    .add(Expr::exists(holds_state).not()),
            )
            .order_by_asc(subscription::Column::CreatedAt)
            .order_by_asc(subscription::Column::Id)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(conn)
            .await
            .map_err(db_error)?;
        Ok(row.map(subscription_of))
    }

    async fn silent_subscriptions<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
        tenant: &str,
    ) -> Result<Vec<Subscription>, DomainError> {
        let rows = SubscriptionEntity::find()
            .filter(subscription::Column::Key.eq(key))
            .filter(subscription::Column::Tenant.eq(tenant))
            .filter(subscription::Column::SilentSince.is_not_null())
            .order_by_asc(subscription::Column::Subscriber)
            .order_by_asc(subscription::Column::CreatedAt)
            .order_by_asc(subscription::Column::Id)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(subscription_of).collect())
    }

    async fn unsubscribe<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
    ) -> Result<bool, DomainError> {
        let scope = AccessScope::allow_all();
        // The state first, then the subscription: the order every writer of
        // both keeps. Nothing is reported for the state.
        StateEntity::delete_many()
            .filter(state_is(instance))
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        let existed = SubscriptionEntity::delete_many()
            .filter(subscription_is(instance))
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?
            .rows_affected
            > 0;
        Ok(existed)
    }

    async fn delete_key<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
    ) -> Result<(usize, usize), DomainError> {
        let scope = AccessScope::allow_all();
        // Every state of the key, locked in the canonical order, then every
        // subscription.
        let states = StateEntity::delete_many()
            .filter(locked_in_order(
                activation_state::Column::Key.eq(key),
                LockType::Update,
            ))
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?
            .rows_affected;
        let subscriptions = SubscriptionEntity::delete_many()
            .filter(subscription::Column::Key.eq(key))
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?
            .rows_affected;
        Ok((count_of(subscriptions)?, count_of(states)?))
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
        // subscriptions that still exist are cleared — silent ones included, so
        // an instance the insert below leaves out keeps no row of an earlier
        // change beside its siblings' new ones. The rows are locked in the
        // canonical order ([`locked_in_order`]), the order of every other
        // multi-row statement over the table, such as a stamp of the same
        // rows running at once.
        let mut still_subscribed = Query::select();
        still_subscribed
            .expr(Expr::val(1))
            .from(SubscriptionEntity)
            .and_where(
                Expr::col((SubscriptionEntity, subscription::Column::Id)).is_in(ids.clone()),
            );
        same_instance(&mut still_subscribed);
        // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-11
        StateEntity::delete_many()
            .filter(locked_in_order(
                Expr::exists(still_subscribed),
                LockType::Update,
            ))
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-11
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-21
        // A subscription of the read set unsubscribed before this statement is not in
        // `subscriptions` for the select to find, and gets no row; a silent one gets
        // none while a sibling of its subscriber at the key and tenant is expected.
        toolkit_db::secure::secure_insert_from_select::<StateEntity, _>(
            [
                activation_state::Column::Subscriber,
                activation_state::Column::InstanceId,
                activation_state::Column::Key,
                activation_state::Column::Tenant,
                activation_state::Column::ChangeRef,
                activation_state::Column::ChangedAt,
                activation_state::Column::ExpectedValue,
                activation_state::Column::Status,
                activation_state::Column::Deliveries,
                activation_state::Column::UpdatedAt,
            ],
            source(write, &ids, now()),
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
            .order_by_asc(activation_state::Column::InstanceId)
            .secure()
            .scope_with(&scope)
            .all(conn)
            .await
            .map_err(db_error)?;
        let mut pairs: Vec<SubscriberTenant> = Vec::new();
        for row in written {
            let pair = SubscriberTenant {
                subscriber: row.subscriber,
                tenant: row.tenant,
            };
            // Ordered by the pair, so a repeat is the previous entry.
            if pairs.last() != Some(&pair) {
                pairs.push(pair);
            }
        }
        Ok(pairs)
    }

    async fn state<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
    ) -> Result<Option<ActivationState>, DomainError> {
        let row = StateEntity::find()
            .filter(state_is(instance))
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
        // `idx_activation_key_tenant`, in subscriber and instance order; a
        // failed read is the store being unavailable, never an absent state.
        let rows = StateEntity::find()
            .filter(activation_state::Column::Key.eq(key))
            .filter(activation_state::Column::Tenant.eq(tenant))
            .order_by_asc(activation_state::Column::Subscriber)
            .order_by_asc(activation_state::Column::InstanceId)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(|err| {
                DomainError::dependency_unavailable("activation store", "read the states", err)
            })?;
        rows.into_iter().map(state_of).collect()
    }

    async fn activation_rows<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
        tenant: &str,
    ) -> Result<Vec<ActivationRow>, DomainError> {
        // One statement: the state's own columns the read shows and the label
        // of its subscription by an outer join on the instance's identity, so a
        // row whose subscription is absent is kept without one. No expected or
        // applied value is selected.
        let unavailable = |err: toolkit_db::secure::ScopeError| {
            DomainError::dependency_unavailable("activation store", "read the states", err)
        };
        let rows = StateEntity::find()
            .filter(activation_state::Column::Key.eq(key))
            .filter(activation_state::Column::Tenant.eq(tenant))
            .order_by_asc(activation_state::Column::Subscriber)
            .order_by_asc(activation_state::Column::InstanceId)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .project_all(conn, |q| {
                let mut q = q.select_only();
                for column in [
                    activation_state::Column::Subscriber,
                    activation_state::Column::InstanceId,
                    activation_state::Column::ChangeRef,
                    activation_state::Column::Status,
                    activation_state::Column::SilentSince,
                    activation_state::Column::ReportedAt,
                    activation_state::Column::Detail,
                ] {
                    q = q.column(column);
                }
                q = q.column_as(
                    Expr::col((SubscriptionEntity, subscription::Column::InstanceLabel)),
                    "instance_label",
                );
                let mut on = Condition::all();
                for (sub, state) in [
                    (
                        subscription::Column::Subscriber,
                        activation_state::Column::Subscriber,
                    ),
                    (
                        subscription::Column::InstanceId,
                        activation_state::Column::InstanceId,
                    ),
                    (subscription::Column::Key, activation_state::Column::Key),
                    (
                        subscription::Column::Tenant,
                        activation_state::Column::Tenant,
                    ),
                ] {
                    on = on.add(Expr::col((SubscriptionEntity, sub)).equals((StateEntity, state)));
                }
                sea_orm::QueryTrait::query(&mut q).join(
                    sea_orm::JoinType::LeftJoin,
                    SubscriptionEntity,
                    on,
                );
                q.into_model::<ActivationRowModel>()
            })
            .await
            .map_err(unavailable)?;
        rows.into_iter()
            .map(|row| {
                let status =
                    ActivationStatus::parse(&row.status).ok_or_else(|| DomainError::Internal {
                        diagnostic: format!(
                            "activation state of `{key}` carries an unknown status `{}`",
                            row.status
                        ),
                    })?;
                Ok(ActivationRow {
                    subscriber: row.subscriber,
                    instance_id: row.instance_id,
                    instance_label: row.instance_label,
                    change_ref: row.change_ref,
                    status,
                    silent_since: row.silent_since,
                    reported_at: row.reported_at,
                    detail: row.detail,
                })
            })
            .collect()
    }

    async fn report<C: DBRunner>(
        &self,
        conn: &C,
        transition: &Transition,
    ) -> Result<Option<OffsetDateTime>, DomainError> {
        #[cfg(any(test, feature = "integration"))]
        crate::infra::storage::activation_hold::reached(
            crate::infra::storage::activation_hold::Step::Report,
            &transition.key,
        )
        .await;
        let at = now();
        let instance = InstanceKey {
            subscriber: transition.subscriber.clone(),
            instance_id: transition.instance_id,
            key: transition.key.clone(),
            tenant: transition.tenant.clone(),
        };
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
            .filter(state_is(&instance))
            // The row guard: the `change_ref` the decision read, still awaiting
            // or silent — a silent row is not terminal.
            .filter(activation_state::Column::ChangeRef.eq(transition.change_ref))
            .filter(activation_state::Column::Status.is_in([
                ActivationStatus::Awaiting.as_str(),
                ActivationStatus::Silent.as_str(),
            ]))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok((updated.rows_affected > 0).then_some(at))
    }

    async fn rearm<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
        change_ref: Uuid,
    ) -> Result<bool, DomainError> {
        #[cfg(any(test, feature = "integration"))]
        crate::infra::storage::activation_hold::reached(
            crate::infra::storage::activation_hold::Step::Rearm,
            &instance.key,
        )
        .await;
        let updated = StateEntity::update_many()
            .col_expr(
                activation_state::Column::Status,
                Expr::value(ActivationStatus::Awaiting.as_str()),
            )
            .col_expr(activation_state::Column::Deliveries, Expr::value(0))
            .col_expr(
                activation_state::Column::SilentSince,
                Expr::value(Option::<OffsetDateTime>::None),
            )
            .col_expr(
                activation_state::Column::LastNotifiedAt,
                Expr::value(Option::<OffsetDateTime>::None),
            )
            .col_expr(activation_state::Column::UpdatedAt, Expr::value(now()))
            .filter(state_is(instance))
            .filter(activation_state::Column::ChangeRef.eq(change_ref))
            .filter(activation_state::Column::Status.eq(ActivationStatus::Silent.as_str()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok(updated.rows_affected > 0)
    }

    async fn clear_silent<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
    ) -> Result<bool, DomainError> {
        #[cfg(any(test, feature = "integration"))]
        crate::infra::storage::activation_hold::reached(
            crate::infra::storage::activation_hold::Step::ClearSilent,
            &instance.key,
        )
        .await;
        let scope = AccessScope::allow_all();
        // First the silent subscription's row, in a statement of its own: a
        // call of silence in flight holds it — whether or not it had to mark
        // it — so this waits for the call's commit. The guarded clear below
        // is a new statement and reads what that commit wrote; within one
        // statement the guard would read the instance's states as they were
        // before the call. A subscription not silent is not taken: there is
        // nothing to clear. `SQLite` renders no lock clause.
        SubscriptionEntity::find()
            .filter(subscription_is(instance))
            .filter(subscription::Column::SilentSince.is_not_null())
            .lock_exclusive()
            .secure()
            .scope_with(&scope)
            .one(conn)
            .await
            .map_err(db_error)?;
        // The guard reads the instance's states at the statement's start: a
        // `silent` one — a call of silence committed meanwhile — keeps the
        // subscription silent.
        let silent_state = Query::select()
            .expr(Expr::val(1))
            .from(StateEntity)
            .cond_where(
                state_is(instance)
                    .add(activation_state::Column::Status.eq(ActivationStatus::Silent.as_str())),
            )
            .to_owned();
        let updated = SubscriptionEntity::update_many()
            .col_expr(
                subscription::Column::SilentSince,
                Expr::value(Option::<OffsetDateTime>::None),
            )
            .filter(subscription_is(instance))
            .filter(subscription::Column::SilentSince.is_not_null())
            .filter(Expr::exists(silent_state).not())
            .secure()
            .scope_with(&scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok(updated.rows_affected > 0)
    }
}

#[cfg(test)]
#[path = "activation_repo_tests.rs"]
mod activation_repo_tests;
