//! Fact intake's tables (feature 06): `bss_rating__fact` (insert-only), `bss_rating__fact_head`
//! (updated under its row lock), `bss_rating__child_window`, `bss_rating__window_schedule` and
//! `bss_rating__usage_policy_projection` (insert-only).

use async_trait::async_trait;
use bss_rating_core::engine::current::input::RatingWindowKind;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ColumnTrait, Condition, DbErr, EntityTrait, ExprTrait, FromQueryResult, QuerySelect, Set,
};
use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;

use crate::domain::child::{ChildRepo, ChildStatus, ChildWork, NewChild, kind_name};
use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::fact::model::{Coverage, FactKind, FactVersion};
use crate::domain::fact::{
    BindingState, FactHead, FactRepo, NewSchedule, ProjectionRepo, ProjectionRow, ScheduleRepo,
    StoredVersion,
};
use crate::infra::storage::entity::{
    child_window, fact, fact_head, usage_policy_projection, window_schedule,
};
use crate::infra::storage::{corrupt, scope_failure};

/// The repository of fact intake's tables.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrmFactRepo;

fn not_inserted(err: &ScopeError) -> bool {
    matches!(err, ScopeError::Db(DbErr::RecordNotInserted))
}

/// The stored name of a rating window.
#[must_use]
pub const fn window_name(window: RatingWindowKind) -> &'static str {
    match window {
        RatingWindowKind::BillingCycle => "billing_cycle",
        RatingWindowKind::CalendarHourUtc => "calendar_hour",
    }
}

/// The timeline column: the slices with their coverage, terms and lots.
fn timeline_json(version: &FactVersion) -> Value {
    Value::Array(
        version
            .slices
            .iter()
            .map(|slice| {
                let reason = match &slice.coverage {
                    Coverage::Chargeable => None,
                    Coverage::Nonchargeable { reason } | Coverage::Unresolved { reason } => {
                        Some(reason.clone())
                    }
                };
                json!({
                    "from": slice.span.start.to_string(),
                    "to": slice.span.end.to_string(),
                    "coverage": slice.coverage.as_str(),
                    "reason": reason,
                    "terms": slice.terms,
                    "lots": slice.lots.iter().map(|lot| json!({
                        "lot_id": lot.lot_id,
                        "quantity": lot.quantity,
                        "unit": lot.unit,
                        "terms_ref": lot.terms_ref,
                        "purchase_origin": lot.purchase_origin,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// The accepted-terms column: per `terms_ref` the plan, revision, cell and binding digest.
fn terms_json(version: &FactVersion) -> Value {
    Value::Array(
        version
            .terms
            .iter()
            .map(|term| {
                json!({
                    "terms_ref": term.terms_ref,
                    "catalog_tenant_id": term.catalog_tenant_id,
                    "plan_id": term.plan_id,
                    "revision_id": term.revision_id,
                    "item_id": term.binding.item_id,
                    "dimension_value": term.binding.dimension_value,
                    "price_id": term.binding.price.price_id,
                    "binding_digest": term.binding_digest.as_str(),
                })
            })
            .collect(),
    )
}

async fn insert_version(
    runner: &impl DBRunner,
    scope: &AccessScope,
    version: &StoredVersion<'_>,
) -> Result<bool, DomainError> {
    let v = version.fact;
    let active = fact::ActiveModel {
        tenant_id: Set(v.fact.tenant_id),
        fact_id: Set(v.fact.fact_id),
        fact_version: Set(v.fact.fact_version),
        entry_id: Set(version.entry_id),
        kind: Set(v.kind.as_str().to_owned()),
        obligation: Set(v.obligation.as_str().to_owned()),
        predecessor_version: Set(v.predecessor_version),
        subscription_id: Set(v.subscription_id),
        sub_line_key: Set(v.sub_line_key.clone()),
        item_id: Set(v.item_id),
        billing_group_id: Set(v.billing_group_id),
        seller_tenant_id: Set(v.axes.seller_tenant_id),
        payer_tenant_id: Set(v.axes.payer_tenant_id),
        resource_tenant_id: Set(v.axes.resource_tenant_id),
        currency: Set(v.currency.clone()),
        period_start: Set(v.period.start),
        period_end: Set(v.period.end),
        served_from: Set(v.served.start),
        served_to: Set(v.served.end),
        timing: Set(v.timing.as_str().to_owned()),
        occurrence_id: Set(v.occurrence_id.clone()),
        billing_terms: Set(v.billing_terms.clone()),
        suspensions: Set(json!({
            "ranges": v.suspensions.ranges.iter().map(|range| json!({
                "start": range.start.to_string(),
                "end": range.end.to_string(),
            })).collect::<Vec<_>>(),
            "posture": v.suspensions.posture,
        })),
        timeline: Set(timeline_json(v)),
        accepted_terms: Set(terms_json(v)),
        producer_digest: Set(v.producer_digest.clone()),
        payload_digest: Set(version.payload_digest.as_str().to_owned()),
        payload: Set(version.payload.clone()),
        received_at: Set(version.received_at),
    };
    let inserted = fact::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("insert fact version", e))?
        .on_conflict_raw(
            OnConflict::columns([
                fact::Column::TenantId,
                fact::Column::FactId,
                fact::Column::FactVersion,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => Ok(true),
        Err(e) if not_inserted(&e) => Ok(false),
        Err(e) => Err(scope_failure("insert fact version", e)),
    }
}

#[derive(FromQueryResult)]
struct PayloadRow {
    payload: Value,
}

async fn version_payload(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    fact_id: Uuid,
    fact_version: i64,
) -> Result<Option<Value>, DomainError> {
    let rows = fact::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(fact::Column::TenantId.eq(tenant_id))
                .add(fact::Column::FactId.eq(fact_id))
                .add(fact::Column::FactVersion.eq(fact_version)),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(fact::Column::Payload)
                .limit(1)
                .into_model::<PayloadRow>()
        })
        .await
        .map_err(|e| scope_failure("read fact version", e))?;
    Ok(rows.into_iter().next().map(|row| row.payload))
}

const HEAD: &str = "bss_rating__fact_head";

fn head_condition(tenant_id: Uuid, fact_id: Uuid) -> Condition {
    Condition::all()
        .add(fact_head::Column::TenantId.eq(tenant_id))
        .add(fact_head::Column::FactId.eq(fact_id))
}

fn head_to_domain(row: fact_head::Model) -> Result<FactHead, DomainError> {
    let kind = FactKind::parse(&row.kind).ok_or_else(|| corrupt(HEAD, "kind", &row.kind))?;
    let binding = BindingState::parse(&row.binding_state, row.binding_reason.as_deref())
        .ok_or_else(|| corrupt(HEAD, "binding_state", &row.binding_state))?;
    let expected_manifest_digest = row
        .expected_manifest_digest
        .as_deref()
        .map(|hex| {
            Sha256Hex::parse(hex).map_err(|_| corrupt(HEAD, "expected_manifest_digest", hex))
        })
        .transpose()?;
    Ok(FactHead {
        tenant_id: row.tenant_id,
        fact_id: row.fact_id,
        shard: row.shard,
        kind,
        current_version: row.current_version,
        binding,
        bound_version: row.bound_version,
        binding_next_check_at: row.binding_next_check_at,
        expected_manifest_digest,
        rollup_due_at: row.rollup_due_at,
        currency: row.currency,
        received_at: row.received_at,
        updated_at: row.updated_at,
    })
}

fn head_model(head: &FactHead) -> fact_head::ActiveModel {
    fact_head::ActiveModel {
        tenant_id: Set(head.tenant_id),
        fact_id: Set(head.fact_id),
        shard: Set(head.shard),
        kind: Set(head.kind.as_str().to_owned()),
        current_version: Set(head.current_version),
        binding_state: Set(head.binding.state().to_owned()),
        binding_reason: Set(head.binding.reason().map(str::to_owned)),
        bound_version: Set(head.bound_version),
        binding_next_check_at: Set(head.binding_next_check_at),
        expected_manifest_digest: Set(head
            .expected_manifest_digest
            .as_ref()
            .map(|digest| digest.as_str().to_owned())),
        rollup_due_at: Set(head.rollup_due_at),
        currency: Set(head.currency.clone()),
        received_at: Set(head.received_at),
        updated_at: Set(head.updated_at),
    }
}

async fn find_head(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    fact_id: Uuid,
    lock: bool,
) -> Result<Option<FactHead>, DomainError> {
    let query = fact_head::Entity::find().secure().scope_with(scope);
    let query = if lock { query.lock_exclusive() } else { query };
    query
        .filter(head_condition(tenant_id, fact_id))
        .one(runner)
        .await
        .map_err(|e| scope_failure("read fact head", e))?
        .map(head_to_domain)
        .transpose()
}

async fn insert_head(
    runner: &impl DBRunner,
    scope: &AccessScope,
    head: &FactHead,
) -> Result<(), DomainError> {
    let active = head_model(head);
    fact_head::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("insert fact head", e))?
        .exec(runner)
        .await
        .map_err(|e| scope_failure("insert fact head", e))?;
    Ok(())
}

async fn update_head(
    runner: &impl DBRunner,
    scope: &AccessScope,
    head: &FactHead,
) -> Result<(), DomainError> {
    let result = fact_head::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            fact_head::Column::CurrentVersion,
            Expr::value(head.current_version),
        )
        .col_expr(fact_head::Column::Kind, Expr::value(head.kind.as_str()))
        .col_expr(
            fact_head::Column::BindingState,
            Expr::value(head.binding.state()),
        )
        .col_expr(
            fact_head::Column::BindingReason,
            Expr::value(head.binding.reason()),
        )
        .col_expr(
            fact_head::Column::BoundVersion,
            Expr::value(head.bound_version),
        )
        .col_expr(
            fact_head::Column::BindingNextCheckAt,
            Expr::value(head.binding_next_check_at),
        )
        .col_expr(
            fact_head::Column::ExpectedManifestDigest,
            Expr::value(
                head.expected_manifest_digest
                    .as_ref()
                    .map(|digest| digest.as_str().to_owned()),
            ),
        )
        .col_expr(
            fact_head::Column::RollupDueAt,
            Expr::value(head.rollup_due_at),
        )
        .col_expr(
            fact_head::Column::Currency,
            Expr::value(head.currency.as_str()),
        )
        .col_expr(fact_head::Column::ReceivedAt, Expr::value(head.received_at))
        .col_expr(fact_head::Column::UpdatedAt, Expr::value(head.updated_at))
        .filter(head_condition(head.tenant_id, head.fact_id))
        .exec(runner)
        .await
        .map_err(|e| scope_failure("update fact head", e))?;
    if result.rows_affected == 1 {
        Ok(())
    } else {
        Err(DomainError::Concurrent(
            "the fact head was not found under its lock".to_owned(),
        ))
    }
}

#[async_trait]
impl FactRepo for OrmFactRepo {
    async fn insert_version(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        version: &StoredVersion<'_>,
    ) -> Result<bool, DomainError> {
        insert_version(tx, scope, version).await
    }

    async fn version_payload(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        fact_id: Uuid,
        fact_version: i64,
    ) -> Result<Option<Value>, DomainError> {
        version_payload(tx, scope, tenant_id, fact_id, fact_version).await
    }

    async fn find_head(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<Option<FactHead>, DomainError> {
        find_head(tx, scope, tenant_id, fact_id, false).await
    }

    async fn lock_head(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<Option<FactHead>, DomainError> {
        find_head(tx, scope, tenant_id, fact_id, true).await
    }

    async fn insert_head(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        head: &FactHead,
    ) -> Result<(), DomainError> {
        insert_head(tx, scope, head).await
    }

    async fn update_head(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        head: &FactHead,
    ) -> Result<(), DomainError> {
        update_head(tx, scope, head).await
    }
}

// ---- children ----------------------------------------------------------------------------------

async fn insert_child(
    runner: &impl DBRunner,
    scope: &AccessScope,
    child: &NewChild,
    at: OffsetDateTime,
) -> Result<bool, DomainError> {
    let aggregation_key = serde_json::to_value(&child.aggregation_key).map_err(|e| {
        DomainError::Internal(
            format!("aggregation key: {e}"),
            Some(crate::domain::error::Cause::new(e)),
        )
    })?;
    let active = child_window::ActiveModel {
        child_id: Set(child.child_id),
        tenant_id: Set(child.tenant_id),
        fact_id: Set(child.fact_id),
        shard: Set(child.shard),
        child_kind: Set(kind_name(child.kind).to_owned()),
        subscription_id: Set(child.subscription_id),
        item_id: Set(child.item_id),
        dimension_value: Set(child.dimension_value.clone()),
        aggregation_key: Set(aggregation_key),
        agg_key_digest: Set(child.agg_key_digest.as_str().to_owned()),
        window_group_id: Set(None),
        window_start: Set(child.window.window_start),
        window_end: Set(child.window.window_end),
        served_from: Set(child.window.served_from),
        served_to: Set(child.window.served_to),
        status: Set(child.status.as_str().to_owned()),
        pending_reason: Set(None),
        failure_reason: Set(child.failure.map(|failure| failure.as_str().to_owned())),
        next_check_at: Set(child.next_check_at),
        input_generation: Set(0),
        current_revision: Set(None),
        current_covered_generation: Set(None),
        current_input_digest: Set(None),
        binding_of_record: Set(None),
        binding_resolves: Set(None),
        engine_of_record: Set(None),
        requested_rebind: Set(false),
        requested_engine: Set(None),
        requested_run_id: Set(None),
        provisional_outcome: Set(None),
        last_reason_code: Set(child.failure.map(|failure| failure.as_str().to_owned())),
        queued_at: Set(None),
        created_at: Set(at),
        updated_at: Set(at),
    };
    let inserted = child_window::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("insert child window", e))?
        .on_conflict_raw(OnConflict::new().do_nothing().to_owned())
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => Ok(true),
        Err(e) if not_inserted(&e) => Ok(false),
        Err(e) => Err(scope_failure("insert child window", e)),
    }
}

#[derive(FromQueryResult)]
struct ChildWorkRow {
    child_id: Uuid,
    subscription_id: Uuid,
}

async fn bump_fact(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    fact_id: Uuid,
    at: OffsetDateTime,
) -> Result<Vec<ChildWork>, DomainError> {
    let of_fact = || {
        Condition::all()
            .add(child_window::Column::TenantId.eq(tenant_id))
            .add(child_window::Column::FactId.eq(fact_id))
    };
    // @cpt-begin:cpt-cf-bss-rating-algo-schedule-new-version:p1:inst-nv-no-delete
    // Updates only: a newer version deletes no child.
    child_window::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            child_window::Column::InputGeneration,
            Expr::col(child_window::Column::InputGeneration).add(1),
        )
        .col_expr(child_window::Column::UpdatedAt, Expr::value(at))
        .filter(of_fact())
        .exec(runner)
        .await
        .map_err(|e| scope_failure("bump the children of a fact", e))?;
    // @cpt-begin:cpt-cf-bss-rating-state-schedule-child-creation:p1:inst-cc-stale
    child_window::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            child_window::Column::Status,
            Expr::value(ChildStatus::Final.bumped().as_str()),
        )
        .filter(of_fact().add(child_window::Column::Status.eq(ChildStatus::Final.as_str())))
        .exec(runner)
        .await
        .map_err(|e| scope_failure("mark final children stale", e))?;
    // @cpt-end:cpt-cf-bss-rating-state-schedule-child-creation:p1:inst-cc-stale
    // @cpt-end:cpt-cf-bss-rating-algo-schedule-new-version:p1:inst-nv-no-delete
    let rows = child_window::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(of_fact())
        .project_all(runner, |q| {
            q.select_only()
                .column(child_window::Column::ChildId)
                .column(child_window::Column::SubscriptionId)
                .into_model::<ChildWorkRow>()
        })
        .await
        .map_err(|e| scope_failure("list the children of a fact", e))?;
    Ok(rows
        .into_iter()
        .map(|row| ChildWork {
            tenant_id,
            child_id: row.child_id,
            subscription_id: row.subscription_id,
        })
        .collect())
}

#[async_trait]
impl ChildRepo for OrmFactRepo {
    async fn insert_if_absent(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        child: &NewChild,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        insert_child(tx, scope, child, at).await
    }

    async fn bump_fact(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        fact_id: Uuid,
        at: OffsetDateTime,
    ) -> Result<Vec<ChildWork>, DomainError> {
        bump_fact(tx, scope, tenant_id, fact_id, at).await
    }
}

// ---- schedules and projections -----------------------------------------------------------------

fn to_i64(field: &'static str, value: u64) -> Result<i64, DomainError> {
    i64::try_from(value).map_err(|_| DomainError::InvalidArgument {
        field,
        detail: format!("{value} exceeds a stored bigint"),
        reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
    })
}

async fn upsert_schedule(
    runner: &impl DBRunner,
    scope: &AccessScope,
    schedule: &NewSchedule,
    at: OffsetDateTime,
) -> Result<(), DomainError> {
    let expected_count = to_i64(
        "expected_count",
        u64::try_from(schedule.expected_count).unwrap_or(u64::MAX),
    )?;
    let active = window_schedule::ActiveModel {
        tenant_id: Set(schedule.tenant_id),
        fact_id: Set(schedule.fact_id),
        item_id: Set(schedule.item_id),
        shard: Set(schedule.shard),
        state: Set("policy_pending".to_owned()),
        fact_version: Set(schedule.fact_version),
        window_policy: Set(window_name(schedule.window_policy).to_owned()),
        usage_policy_id: Set(schedule.usage_policy_id),
        usage_policy_version: Set(to_i64(
            "usage_policy_version",
            schedule.usage_policy_version,
        )?),
        usage_policy_digest: Set(schedule.usage_policy_digest.clone()),
        finalization_policy_id: Set(None),
        finalization_policy_version: Set(None),
        finalization_policy_owner_scope: Set(None),
        delay_seconds: Set(None),
        evidence_mode: Set(None),
        next_due_window_start: Set(None),
        next_due_at: Set(schedule.next_due_at),
        expected_count: Set(expected_count),
        scheduler_version: Set(0),
        updated_at: Set(at),
    };
    let inserted = window_schedule::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("insert window schedule", e))?
        .on_conflict_raw(OnConflict::new().do_nothing().to_owned())
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => return Ok(()),
        Err(e) if not_inserted(&e) => {}
        Err(e) => return Err(scope_failure("insert window schedule", e)),
    }
    // The schedule exists: a newer version raises its `fact_version` and expected count; the
    // pinned policy, the cursor and the state are the scheduler's and stay.
    window_schedule::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            window_schedule::Column::FactVersion,
            Expr::value(schedule.fact_version),
        )
        .col_expr(
            window_schedule::Column::ExpectedCount,
            Expr::value(expected_count),
        )
        .col_expr(window_schedule::Column::UpdatedAt, Expr::value(at))
        .filter(
            Condition::all()
                .add(window_schedule::Column::TenantId.eq(schedule.tenant_id))
                .add(window_schedule::Column::FactId.eq(schedule.fact_id))
                .add(window_schedule::Column::ItemId.eq(schedule.item_id))
                .add(window_schedule::Column::FactVersion.lt(schedule.fact_version)),
        )
        .exec(runner)
        .await
        .map_err(|e| scope_failure("raise the window schedule's version", e))?;
    Ok(())
}

async fn insert_projection(
    runner: &impl DBRunner,
    scope: &AccessScope,
    row: &ProjectionRow,
    at: OffsetDateTime,
) -> Result<(), DomainError> {
    let inputs = serde_json::to_value(&row.inputs).map_err(|e| {
        DomainError::Internal(
            format!("projection inputs: {e}"),
            Some(crate::domain::error::Cause::new(e)),
        )
    })?;
    let active = usage_policy_projection::ActiveModel {
        tenant_id: Set(row.tenant_id),
        subscription_id: Set(row.subscription_id),
        item_id: Set(row.item_id),
        valid_from: Set(row.valid_from),
        fact_version: Set(row.fact_version),
        valid_to: Set(row.valid_to),
        fact_id: Set(row.fact_id),
        sub_line_key: Set(row.sub_line_key.clone()),
        policy_id: Set(row.policy_id),
        policy_version: Set(to_i64("policy_version", row.policy_version)?),
        policy_digest: Set(row.policy_digest.clone()),
        rating_window: Set(window_name(row.rating_window).to_owned()),
        aggregation_scope: Set("subscription_line".to_owned()),
        meter: Set(row.meter.clone()),
        inputs: Set(inputs),
        binding_digest: Set(row.binding_digest.as_str().to_owned()),
        created_at: Set(at),
    };
    let inserted = usage_policy_projection::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("insert usage policy projection", e))?
        .on_conflict_raw(OnConflict::new().do_nothing().to_owned())
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => Ok(()),
        Err(e) if not_inserted(&e) => Ok(()),
        Err(e) => Err(scope_failure("insert usage policy projection", e)),
    }
}

#[async_trait]
impl ScheduleRepo for OrmFactRepo {
    async fn upsert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        schedule: &NewSchedule,
        at: OffsetDateTime,
    ) -> Result<(), DomainError> {
        upsert_schedule(tx, scope, schedule, at).await
    }
}

#[async_trait]
impl ProjectionRepo for OrmFactRepo {
    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        row: &ProjectionRow,
        at: OffsetDateTime,
    ) -> Result<(), DomainError> {
        insert_projection(tx, scope, row, at).await
    }
}

/// Reads of fact intake's tables for tests: every row of a tenant, every column.
#[cfg(any(test, feature = "test-providers"))]
pub mod test_reads {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, Condition, EntityTrait};
    use toolkit_db::secure::{AccessScope, DBRunner, SecureEntityExt, SecureUpdateExt};
    use uuid::Uuid;

    use crate::domain::error::DomainError;
    use crate::domain::fact::FactHead;
    use crate::infra::storage::entity::{
        child_window, fact, pricing_binding, pricing_resolve, usage_policy_projection,
        window_schedule,
    };
    use crate::infra::storage::scope_failure;

    /// The head of `fact_id`.
    ///
    /// # Errors
    ///
    /// A storage failure or a corrupt row.
    pub async fn head(
        runner: &impl DBRunner,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<Option<FactHead>, DomainError> {
        super::find_head(runner, &AccessScope::allow_all(), tenant_id, fact_id, false).await
    }

    /// The stored versions of `fact_id`, in order.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub async fn versions(
        runner: &impl DBRunner,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<Vec<i64>, DomainError> {
        let mut versions: Vec<i64> = fact::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(fact::Column::TenantId.eq(tenant_id))
                    .add(fact::Column::FactId.eq(fact_id)),
            )
            .all(runner)
            .await
            .map_err(|e| scope_failure("list fact versions", e))?
            .into_iter()
            .map(|row| row.fact_version)
            .collect();
        versions.sort_unstable();
        Ok(versions)
    }

    /// The children of `fact_id`.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub async fn children(
        runner: &impl DBRunner,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<Vec<child_window::Model>, DomainError> {
        child_window::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(child_window::Column::TenantId.eq(tenant_id))
                    .add(child_window::Column::FactId.eq(fact_id)),
            )
            .all(runner)
            .await
            .map_err(|e| scope_failure("list children", e))
    }

    /// Sets a child's status, as the rater would.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub async fn set_child_status(
        runner: &impl DBRunner,
        child_id: Uuid,
        status: &str,
    ) -> Result<(), DomainError> {
        child_window::Entity::update_many()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .col_expr(child_window::Column::Status, Expr::value(status))
            .filter(Condition::all().add(child_window::Column::ChildId.eq(child_id)))
            .exec(runner)
            .await
            .map_err(|e| scope_failure("set child status", e))?;
        Ok(())
    }

    /// The schedules of `fact_id`.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub async fn schedules(
        runner: &impl DBRunner,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<Vec<window_schedule::Model>, DomainError> {
        window_schedule::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(window_schedule::Column::TenantId.eq(tenant_id))
                    .add(window_schedule::Column::FactId.eq(fact_id)),
            )
            .all(runner)
            .await
            .map_err(|e| scope_failure("list schedules", e))
    }

    /// The usage policy projection rows of `tenant_id`.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub async fn projections(
        runner: &impl DBRunner,
        tenant_id: Uuid,
    ) -> Result<Vec<usage_policy_projection::Model>, DomainError> {
        usage_policy_projection::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(Condition::all().add(usage_policy_projection::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| scope_failure("list projections", e))
    }

    /// The stored answers of `tenant_id`.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub async fn answers(
        runner: &impl DBRunner,
        tenant_id: Uuid,
    ) -> Result<Vec<pricing_binding::Model>, DomainError> {
        pricing_binding::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(Condition::all().add(pricing_binding::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| scope_failure("list pricing answers", e))
    }

    /// The stored questions of `tenant_id`.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub async fn resolves(
        runner: &impl DBRunner,
        tenant_id: Uuid,
    ) -> Result<Vec<pricing_resolve::Model>, DomainError> {
        pricing_resolve::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(Condition::all().add(pricing_resolve::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| scope_failure("list pricing resolves", e))
    }
}
