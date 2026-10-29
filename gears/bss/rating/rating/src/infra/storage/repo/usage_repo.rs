//! Feature 04's tables: `bss_rating__source_checkpoint` (platform-scoped, locked `FOR UPDATE` by
//! the page and restart transactions), `bss_rating__usage_record` (the capture insert and the
//! conditional contribution-state updates, each under the row tenant's scope),
//! `bss_rating__usage_capture_reject` (platform-scoped) and `bss_rating__source_loss`.

use std::collections::BTreeMap;

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, FromQueryResult, QuerySelect, Set};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;

use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::usage::entry::{DigestVersion, EntryIdentity};
use crate::domain::usage::feed::FeedCursor;
use crate::domain::usage::loss::{LossKind, LossState, NewSourceLoss, SourceLoss, TaintedDays};
use crate::domain::usage::normalize::{
    EntryType, NaturalKey, NormalizedUsage, RejectReason, UsageSubject,
};
use crate::domain::usage::source::{
    Checkpoint, SourceDefinition, SourceId, SourceState, Transition,
};
use crate::domain::usage::{
    CaptureReject, CaptureRejectKind, CheckpointRepo, ContributionState, NewUsageRecord,
    SourceLossRepo, StoredDigest, StoredUsageRecord, UsageRecordRepo,
};
use crate::infra::storage::entity::{
    source_checkpoint, source_loss, usage_capture_reject, usage_record,
};
use crate::infra::storage::{corrupt, scope_failure};

const CHECKPOINT_TABLE: &str = "bss_rating__source_checkpoint";
const RECORD_TABLE: &str = "bss_rating__usage_record";
const LOSS_TABLE: &str = "bss_rating__source_loss";

/// The `source_kind` of a usage source.
const USAGE_KIND: &str = "usage";

/// The scope of the platform-scoped tables (checkpoints, capture rejects): a source spans tenants
/// and its rows hold no tenant's data, so they are filtered by their explicit keys alone.
fn platform_scope() -> AccessScope {
    AccessScope::allow_all()
}

/// Whether an insert with `ON CONFLICT … DO NOTHING` inserted nothing.
fn not_inserted(err: &ScopeError) -> bool {
    matches!(err, ScopeError::Db(DbErr::RecordNotInserted))
}

// ---- checkpoints -------------------------------------------------------------------------------

/// Every usage source's checkpoint, locked for the transaction (`FOR UPDATE`; SQLite serializes
/// writers instead).
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn lock_usage_checkpoints(
    runner: &impl DBRunner,
) -> Result<Vec<Checkpoint>, DomainError> {
    source_checkpoint::Entity::find()
        .secure()
        .scope_with(&platform_scope())
        .lock_exclusive()
        .filter(Condition::all().add(source_checkpoint::Column::SourceKind.eq(USAGE_KIND)))
        .all(runner)
        .await
        .map_err(|e| scope_failure("lock usage checkpoints", e))?
        .into_iter()
        .map(checkpoint_to_domain)
        .collect()
}

/// The checkpoint of `source`, locked for the transaction (`FOR UPDATE`; SQLite serializes
/// writers instead).
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn lock_checkpoint(
    runner: &impl DBRunner,
    source: SourceId,
) -> Result<Option<Checkpoint>, DomainError> {
    source_checkpoint::Entity::find()
        .secure()
        .scope_with(&platform_scope())
        .lock_exclusive()
        .filter(Condition::all().add(source_checkpoint::Column::SourceId.eq(source.to_string())))
        .one(runner)
        .await
        .map_err(|e| scope_failure("lock usage checkpoint", e))?
        .map(checkpoint_to_domain)
        .transpose()
}

/// Stores a new checkpoint; `false` when the source has one.
///
/// # Errors
///
/// A storage failure.
pub async fn insert_checkpoint(
    runner: &impl DBRunner,
    checkpoint: &Checkpoint,
) -> Result<bool, DomainError> {
    let active = source_checkpoint::ActiveModel {
        source_id: Set(checkpoint.source_id.to_string()),
        source_kind: Set(USAGE_KIND.to_owned()),
        source_definition: Set(checkpoint.definition.to_json()),
        cursor: Set(checkpoint.cursor.as_ref().map(|c| c.as_str().to_owned())),
        applied_seq: Set(None),
        state: Set(checkpoint.state.as_str().to_owned()),
        updated_at: Set(checkpoint.updated_at),
        config_generation: Set(generation_column(checkpoint.config_generation)?),
    };
    let inserted = source_checkpoint::Entity::insert(active.clone())
        .secure()
        .scope_with_model(&platform_scope(), &active)
        .map_err(|e| scope_failure("insert usage checkpoint", e))?
        .on_conflict_raw(
            OnConflict::column(source_checkpoint::Column::SourceId)
                .do_nothing()
                .to_owned(),
        )
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => Ok(true),
        Err(e) if not_inserted(&e) => Ok(false),
        Err(e) => Err(scope_failure("insert usage checkpoint", e)),
    }
}

/// Writes the cursor of `source`.
///
/// # Errors
///
/// A storage failure.
pub async fn set_cursor(
    runner: &impl DBRunner,
    source: SourceId,
    cursor: Option<&FeedCursor>,
    now: OffsetDateTime,
) -> Result<(), DomainError> {
    source_checkpoint::Entity::update_many()
        .secure()
        .scope_with(&platform_scope())
        .col_expr(
            source_checkpoint::Column::Cursor,
            Expr::value(cursor.map(|c| c.as_str().to_owned())),
        )
        .col_expr(source_checkpoint::Column::UpdatedAt, Expr::value(now))
        .filter(Condition::all().add(source_checkpoint::Column::SourceId.eq(source.to_string())))
        .exec(runner)
        .await
        .map_err(|e| scope_failure("advance usage checkpoint", e))?;
    Ok(())
}

/// Moves `source` along `transition`; `true` when it moved.
///
/// # Errors
///
/// A storage failure.
pub async fn set_source_state(
    runner: &impl DBRunner,
    source: SourceId,
    transition: Transition,
    now: OffsetDateTime,
) -> Result<bool, DomainError> {
    let result = source_checkpoint::Entity::update_many()
        .secure()
        .scope_with(&platform_scope())
        .col_expr(
            source_checkpoint::Column::State,
            Expr::value(transition.to.as_str()),
        )
        .col_expr(source_checkpoint::Column::UpdatedAt, Expr::value(now))
        .filter(
            Condition::all()
                .add(source_checkpoint::Column::SourceId.eq(source.to_string()))
                .add(source_checkpoint::Column::State.eq(transition.from.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| scope_failure("move usage source state", e))?;
    Ok(result.rows_affected == 1)
}

/// Raises every usage checkpoint's `config_generation` below `generation` to it.
///
/// # Errors
///
/// A storage failure, or a generation the column cannot hold.
pub async fn stamp_generation(runner: &impl DBRunner, generation: u64) -> Result<(), DomainError> {
    let generation = generation_column(generation)?;
    source_checkpoint::Entity::update_many()
        .secure()
        .scope_with(&platform_scope())
        .col_expr(
            source_checkpoint::Column::ConfigGeneration,
            Expr::value(generation),
        )
        .filter(
            Condition::all()
                .add(source_checkpoint::Column::SourceKind.eq(USAGE_KIND))
                .add(source_checkpoint::Column::ConfigGeneration.lt(generation)),
        )
        .exec(runner)
        .await
        .map_err(|e| scope_failure("stamp usage configuration generation", e))?;
    Ok(())
}

/// A configuration generation as the `bigint` column holds it.
fn generation_column(generation: u64) -> Result<i64, DomainError> {
    i64::try_from(generation).map_err(|_| DomainError::InvalidArgument {
        field: "config_generation",
        detail: format!("a configuration generation is at most {}", i64::MAX),
        reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
    })
}

fn checkpoint_to_domain(row: source_checkpoint::Model) -> Result<Checkpoint, DomainError> {
    let source_id = SourceId::parse(&row.source_id)
        .ok_or_else(|| corrupt(CHECKPOINT_TABLE, "source_id", &row.source_id))?;
    let definition = SourceDefinition::from_json(&row.source_definition)
        .map_err(|_| corrupt(CHECKPOINT_TABLE, "source_definition", &row.source_id))?;
    if definition.source_id() != source_id {
        return Err(corrupt(
            CHECKPOINT_TABLE,
            "source_definition",
            &row.source_id,
        ));
    }
    let cursor = row
        .cursor
        .map(FeedCursor::parse)
        .transpose()
        .map_err(|_| corrupt(CHECKPOINT_TABLE, "cursor", &row.source_id))?;
    let state = SourceState::parse(&row.state)
        .ok_or_else(|| corrupt(CHECKPOINT_TABLE, "state", &row.state))?;
    let config_generation = u64::try_from(row.config_generation)
        .map_err(|_| corrupt(CHECKPOINT_TABLE, "config_generation", &row.source_id))?;
    Ok(Checkpoint {
        source_id,
        definition,
        cursor,
        state,
        updated_at: row.updated_at,
        config_generation,
    })
}

// ---- usage records -----------------------------------------------------------------------------

fn identity_condition(identity: EntryIdentity) -> Condition {
    Condition::all()
        .add(usage_record::Column::TenantId.eq(identity.tenant_id))
        .add(usage_record::Column::UsageRecordId.eq(identity.usage_record_id))
}

/// The stored row of `identity` within `scope`.
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn find_record(
    runner: &impl DBRunner,
    scope: &AccessScope,
    identity: EntryIdentity,
) -> Result<Option<StoredUsageRecord>, DomainError> {
    usage_record::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(identity_condition(identity))
        .one(runner)
        .await
        .map_err(|e| scope_failure("find usage record", e))?
        .map(record_to_domain)
        .transpose()
}

#[derive(FromQueryResult)]
struct DigestRow {
    tenant_id: Uuid,
    usage_record_id: Uuid,
    raw_sha256: String,
    raw_digest_version: i16,
}

/// The digests of the rows of `tenant_id` among `ids`, within `scope`.
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn stored_digests(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<StoredDigest>, DomainError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = usage_record::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(usage_record::Column::TenantId.eq(tenant_id))
                .add(usage_record::Column::UsageRecordId.is_in(ids.iter().copied())),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(usage_record::Column::TenantId)
                .column(usage_record::Column::UsageRecordId)
                .column(usage_record::Column::RawSha256)
                .column(usage_record::Column::RawDigestVersion)
                .into_model::<DigestRow>()
        })
        .await
        .map_err(|e| scope_failure("read stored usage digests", e))?;
    rows.into_iter()
        .map(|row| {
            let raw_sha256 = Sha256Hex::parse(&row.raw_sha256).map_err(|_| {
                corrupt(RECORD_TABLE, "raw_sha256", &row.usage_record_id.to_string())
            })?;
            Ok(StoredDigest {
                identity: EntryIdentity {
                    tenant_id: row.tenant_id,
                    usage_record_id: row.usage_record_id,
                },
                raw_sha256,
                raw_digest_version: DigestVersion::parse(row.raw_digest_version),
            })
        })
        .collect()
}

/// The stored row of `identity` within `scope`, every column: for tests.
///
/// # Errors
///
/// A storage failure.
#[cfg(any(test, feature = "test-providers"))]
pub async fn find_record_row(
    runner: &impl DBRunner,
    scope: &AccessScope,
    identity: EntryIdentity,
) -> Result<Option<usage_record::Model>, DomainError> {
    usage_record::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(identity_condition(identity))
        .one(runner)
        .await
        .map_err(|e| scope_failure("find usage record", e))
}

/// Every stored row of `tenant_id` within `scope`: for tests.
///
/// # Errors
///
/// A storage failure.
#[cfg(any(test, feature = "test-providers"))]
pub async fn list_records(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
) -> Result<Vec<usage_record::Model>, DomainError> {
    usage_record::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(usage_record::Column::TenantId.eq(tenant_id)))
        .all(runner)
        .await
        .map_err(|e| scope_failure("list usage records", e))
}

/// The capture insert; `false` when a row of the identity is stored.
///
/// # Errors
///
/// [`DomainError::Concurrent`] on a unique violation of the natural key (a concurrent writer);
/// a scope refusal; a storage failure.
pub async fn insert_record(
    runner: &impl DBRunner,
    scope: &AccessScope,
    record: &NewUsageRecord,
) -> Result<bool, DomainError> {
    let outcome = &record.outcome;
    let n = outcome.normalized();
    let withdrawn_by = outcome.withdrawn_by();
    // Every column is set here, so a column added later must be decided for the capture insert.
    let active = usage_record::ActiveModel {
        tenant_id: Set(record.identity.tenant_id),
        usage_record_id: Set(record.identity.usage_record_id),
        source_id: Set(record.source_id.to_string()),
        raw_entry: Set(record.raw_entry.clone()),
        raw_sha256: Set(record.raw_sha256.as_str().to_owned()),
        raw_digest_version: Set(record.raw_digest_version.as_i16()),
        captured_at: Set(record.captured_at),
        gts_type: Set(n.map(|n| n.gts_type.as_str().to_owned())),
        idempotency_key: Set(n.map(|n| n.idempotency_key.clone())),
        resource_type: Set(n.map(|n| n.resource_type.clone())),
        resource_id: Set(n.map(|n| n.resource_id.clone())),
        subject_ref: Set(n.and_then(|n| n.subject.as_ref().map(UsageSubject::to_json))),
        interval_start: Set(n.map(|n| n.interval_start)),
        interval_end: Set(n.map(|n| n.interval_end)),
        quantity: Set(n.map(|n| n.quantity.as_str().to_owned())),
        metadata: Set(n.map(NormalizedUsage::metadata_json)),
        entry_type: Set(n.map(|n| n.entry_type().as_str().to_owned())),
        invalidates_id: Set(n.and_then(|n| n.invalidation().map(|i| i.target))),
        reason_code: Set(n.and_then(|n| n.invalidation().map(|i| i.reason_code.clone()))),
        origin: Set(n.map(|n| n.origin.as_str().to_owned())),
        accepted_at: Set(n.map(|n| n.accepted_at)),
        contribution_state: Set(outcome.state().as_str().to_owned()),
        reject_reason: Set(outcome.reject_reason().map(|r| r.as_str().to_owned())),
        withdrawn_by: Set(withdrawn_by),
        withdrawn_at: Set(withdrawn_by.map(|_| record.captured_at)),
    };
    let inserted = usage_record::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("capture usage record", e))?
        .on_conflict_raw(
            OnConflict::columns([
                usage_record::Column::TenantId,
                usage_record::Column::UsageRecordId,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => Ok(true),
        Err(e) if not_inserted(&e) => Ok(false),
        Err(e) => Err(scope_failure("capture usage record", e)),
    }
}

#[derive(FromQueryResult)]
struct RecordIdRow {
    usage_record_id: Uuid,
}

async fn first_id(
    runner: &impl DBRunner,
    scope: &AccessScope,
    condition: Condition,
    context: &'static str,
) -> Result<Option<Uuid>, DomainError> {
    let rows = usage_record::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(condition)
        .project_all(runner, |q| {
            q.select_only()
                .column(usage_record::Column::UsageRecordId)
                .limit(1)
                .into_model::<RecordIdRow>()
        })
        .await
        .map_err(|e| scope_failure(context, e))?;
    Ok(rows.into_iter().next().map(|row| row.usage_record_id))
}

/// The id of the stored row holding `key`, other than `except`.
///
/// # Errors
///
/// A storage failure.
pub async fn natural_key_holder(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &NaturalKey,
    except: Uuid,
) -> Result<Option<Uuid>, DomainError> {
    let condition = Condition::all()
        .add(usage_record::Column::TenantId.eq(key.tenant_id))
        .add(usage_record::Column::GtsType.eq(key.gts_type.as_str()))
        .add(usage_record::Column::IdempotencyKey.eq(key.idempotency_key.as_str()))
        .add(usage_record::Column::IntervalStart.eq(key.interval_start))
        .add(usage_record::Column::IntervalEnd.eq(key.interval_end))
        .add(usage_record::Column::EntryType.eq(key.entry_type.as_str()))
        .add(usage_record::Column::UsageRecordId.ne(except));
    first_id(runner, scope, condition, "find the natural key's holder").await
}

/// The id of the stored invalidation naming `target`, other than `except`.
///
/// # Errors
///
/// A storage failure.
pub async fn invalidation_naming(
    runner: &impl DBRunner,
    scope: &AccessScope,
    target: EntryIdentity,
    except: Uuid,
) -> Result<Option<Uuid>, DomainError> {
    let condition = Condition::all()
        .add(usage_record::Column::TenantId.eq(target.tenant_id))
        .add(usage_record::Column::InvalidatesId.eq(target.usage_record_id))
        .add(usage_record::Column::EntryType.eq(EntryType::Invalidation.as_str()))
        .add(usage_record::Column::UsageRecordId.ne(except));
    first_id(
        runner,
        scope,
        condition,
        "find the invalidation of a record",
    )
    .await
}

/// The conditional withdrawal; `true` when this call moved the row.
///
/// # Errors
///
/// A storage failure.
pub async fn withdraw(
    runner: &impl DBRunner,
    scope: &AccessScope,
    target: EntryIdentity,
    from: ContributionState,
    by: Uuid,
    at: OffsetDateTime,
) -> Result<bool, DomainError> {
    let result = usage_record::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            usage_record::Column::ContributionState,
            Expr::value(ContributionState::Withdrawn.as_str()),
        )
        .col_expr(usage_record::Column::WithdrawnBy, Expr::value(Some(by)))
        .col_expr(usage_record::Column::WithdrawnAt, Expr::value(Some(at)))
        .filter(
            identity_condition(target)
                .add(usage_record::Column::ContributionState.eq(from.as_str()))
                .add(
                    usage_record::Column::ContributionState.ne(ContributionState::Applied.as_str()),
                )
                .add(usage_record::Column::WithdrawnBy.is_null()),
        )
        .exec(runner)
        .await
        .map_err(|e| scope_failure("withdraw usage record", e))?;
    Ok(result.rows_affected == 1)
}

/// A retried row re-interpreted and moved to `to`; `true` when this call moved it out of
/// `rejected`.
///
/// # Errors
///
/// [`DomainError::Concurrent`] on a natural-key unique violation; a storage failure.
pub async fn complete_retry(
    runner: &impl DBRunner,
    scope: &AccessScope,
    normalized: &NormalizedUsage,
    to: ContributionState,
) -> Result<bool, DomainError> {
    let n = normalized;
    let update = usage_record::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            usage_record::Column::ContributionState,
            Expr::value(to.as_str()),
        )
        .col_expr(
            usage_record::Column::RejectReason,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            usage_record::Column::GtsType,
            Expr::value(n.gts_type.as_str()),
        )
        .col_expr(
            usage_record::Column::IdempotencyKey,
            Expr::value(n.idempotency_key.as_str()),
        )
        .col_expr(
            usage_record::Column::ResourceType,
            Expr::value(n.resource_type.as_str()),
        )
        .col_expr(
            usage_record::Column::ResourceId,
            Expr::value(n.resource_id.as_str()),
        )
        .col_expr(
            usage_record::Column::SubjectRef,
            Expr::value(n.subject.as_ref().map(UsageSubject::to_json)),
        )
        .col_expr(
            usage_record::Column::IntervalStart,
            Expr::value(n.interval_start),
        )
        .col_expr(
            usage_record::Column::IntervalEnd,
            Expr::value(n.interval_end),
        )
        .col_expr(
            usage_record::Column::Quantity,
            Expr::value(n.quantity.as_str()),
        )
        .col_expr(
            usage_record::Column::Metadata,
            Expr::value(n.metadata_json()),
        )
        .col_expr(
            usage_record::Column::EntryType,
            Expr::value(n.entry_type().as_str()),
        )
        .col_expr(
            usage_record::Column::InvalidatesId,
            Expr::value(n.invalidation().map(|i| i.target)),
        )
        .col_expr(
            usage_record::Column::ReasonCode,
            Expr::value(n.invalidation().map(|i| i.reason_code.clone())),
        )
        .col_expr(usage_record::Column::Origin, Expr::value(n.origin.as_str()))
        .col_expr(usage_record::Column::AcceptedAt, Expr::value(n.accepted_at));
    let result = update
        .filter(
            identity_condition(normalized.identity)
                .add(
                    usage_record::Column::ContributionState
                        .eq(ContributionState::Rejected.as_str()),
                )
                .add(usage_record::Column::WithdrawnBy.is_null()),
        )
        .exec(runner)
        .await
        .map_err(|e| scope_failure("complete usage retry", e))?;
    Ok(result.rows_affected == 1)
}

/// A retried row rejected again for `reason`, if it is still rejected and not withdrawn; `true`
/// when this call wrote it.
///
/// # Errors
///
/// A storage failure.
pub async fn reject_again(
    runner: &impl DBRunner,
    scope: &AccessScope,
    identity: EntryIdentity,
    reason: RejectReason,
) -> Result<bool, DomainError> {
    let result = usage_record::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            usage_record::Column::RejectReason,
            Expr::value(Some(reason.as_str().to_owned())),
        )
        .filter(
            identity_condition(identity)
                .add(
                    usage_record::Column::ContributionState
                        .eq(ContributionState::Rejected.as_str()),
                )
                .add(usage_record::Column::WithdrawnBy.is_null()),
        )
        .exec(runner)
        .await
        .map_err(|e| scope_failure("reject usage retry", e))?;
    Ok(result.rows_affected == 1)
}

/// Whether the entry of digest `raw_sha256` is among `source`'s capture rejects.
///
/// # Errors
///
/// A storage failure.
pub async fn capture_rejected(
    runner: &impl DBRunner,
    source: SourceId,
    raw_sha256: &Sha256Hex,
) -> Result<bool, DomainError> {
    let count = usage_capture_reject::Entity::find()
        .secure()
        .scope_with(&platform_scope())
        .filter(
            Condition::all()
                .add(usage_capture_reject::Column::SourceId.eq(source.to_string()))
                .add(usage_capture_reject::Column::RawSha256.eq(raw_sha256.as_str())),
        )
        .count(runner)
        .await
        .map_err(|e| scope_failure("find a capture reject", e))?;
    Ok(count > 0)
}

/// Stores a capture reject; `false` when the entry was stored for the source already.
///
/// # Errors
///
/// A storage failure.
pub async fn insert_capture_reject(
    runner: &impl DBRunner,
    reject: &CaptureReject,
) -> Result<bool, DomainError> {
    let active = usage_capture_reject::ActiveModel {
        source_id: Set(reject.source_id.to_string()),
        raw_sha256: Set(reject.raw_sha256.as_str().to_owned()),
        raw_entry: Set(reject.raw_entry.clone()),
        captured_at: Set(reject.captured_at),
        reason: Set(reject.kind.as_str().to_owned()),
        loss_id: Set(match reject.kind {
            CaptureRejectKind::Unidentifiable { loss_id } => Some(loss_id),
            CaptureRejectKind::OutsideSubscription => None,
        }),
    };
    let inserted = usage_capture_reject::Entity::insert(active.clone())
        .secure()
        .scope_with_model(&platform_scope(), &active)
        .map_err(|e| scope_failure("set a usage entry aside", e))?
        .on_conflict_raw(
            OnConflict::columns([
                usage_capture_reject::Column::SourceId,
                usage_capture_reject::Column::RawSha256,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => Ok(true),
        Err(e) if not_inserted(&e) => Ok(false),
        Err(e) => Err(scope_failure("set a usage entry aside", e)),
    }
}

/// The capture rejects of `source`: for tests.
///
/// # Errors
///
/// A storage failure.
#[cfg(any(test, feature = "test-providers"))]
pub async fn list_capture_rejects(
    runner: &impl DBRunner,
    source: SourceId,
) -> Result<Vec<usage_capture_reject::Model>, DomainError> {
    usage_capture_reject::Entity::find()
        .secure()
        .scope_with(&platform_scope())
        .filter(Condition::all().add(usage_capture_reject::Column::SourceId.eq(source.to_string())))
        .all(runner)
        .await
        .map_err(|e| scope_failure("list capture rejects", e))
}

fn record_to_domain(row: usage_record::Model) -> Result<StoredUsageRecord, DomainError> {
    let id = row.usage_record_id.to_string();
    let raw_sha256 =
        Sha256Hex::parse(&row.raw_sha256).map_err(|_| corrupt(RECORD_TABLE, "raw_sha256", &id))?;
    let state = ContributionState::parse(&row.contribution_state)
        .ok_or_else(|| corrupt(RECORD_TABLE, "contribution_state", &row.contribution_state))?;
    let reject_reason = row
        .reject_reason
        .as_deref()
        .map(|reason| {
            RejectReason::parse(reason)
                .ok_or_else(|| corrupt(RECORD_TABLE, "reject_reason", reason))
        })
        .transpose()?;
    let entry_type = row
        .entry_type
        .as_deref()
        .map(|kind| EntryType::parse(kind).ok_or_else(|| corrupt(RECORD_TABLE, "entry_type", kind)))
        .transpose()?;
    Ok(StoredUsageRecord {
        identity: EntryIdentity {
            tenant_id: row.tenant_id,
            usage_record_id: row.usage_record_id,
        },
        raw_entry: row.raw_entry,
        raw_sha256,
        raw_digest_version: DigestVersion::parse(row.raw_digest_version),
        state,
        reject_reason,
        withdrawn_by: row.withdrawn_by,
        entry_type,
    })
}

// ---- source losses -----------------------------------------------------------------------------

/// Opens `loss`, or returns the not-resolved loss of the same tenant, source, kind and last good
/// cursor. Losses are opened under the source's checkpoint lock, so two writers never race here.
///
/// # Errors
///
/// A storage failure, a scope refusal or a corrupt row.
pub async fn open_loss(
    runner: &impl DBRunner,
    scope: &AccessScope,
    loss: &NewSourceLoss,
) -> Result<SourceLoss, DomainError> {
    let cursor = loss.last_good_cursor.as_ref().map(FeedCursor::as_str);
    let mut condition = Condition::all()
        .add(source_loss::Column::TenantId.eq(loss.tenant_id))
        .add(source_loss::Column::SourceId.eq(loss.source_id.to_string()))
        .add(source_loss::Column::Kind.eq(loss.kind.as_str()))
        .add(source_loss::Column::State.ne(LossState::Resolved.as_str()));
    condition = match cursor {
        Some(cursor) => condition.add(source_loss::Column::LastGoodCursor.eq(cursor)),
        None => condition.add(source_loss::Column::LastGoodCursor.is_null()),
    };
    let existing = source_loss::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(condition)
        .one(runner)
        .await
        .map_err(|e| scope_failure("find open source loss", e))?;
    if let Some(row) = existing {
        return loss_to_domain(row);
    }
    let active = source_loss::ActiveModel {
        tenant_id: Set(loss.tenant_id),
        loss_id: Set(loss.loss_id),
        source_id: Set(loss.source_id.to_string()),
        tenant_scope: Set(Value::from(
            loss.tenant_scope
                .roots()
                .iter()
                .map(|root| Value::String(root.as_hyphenated().to_string()))
                .collect::<Vec<_>>(),
        )),
        kind: Set(loss.kind.as_str().to_owned()),
        gts_type_ids: Set(Value::from(
            loss.meters
                .iter()
                .map(|meter| Value::String(meter.as_str().to_owned()))
                .collect::<Vec<_>>(),
        )),
        detected_at: Set(loss.detected_at),
        last_good_cursor: Set(cursor.map(str::to_owned)),
        last_good_at: Set(loss.last_good_at),
        restart_cursor: Set(loss.restart_cursor.as_ref().map(|c| c.as_str().to_owned())),
        state: Set(LossState::Open.as_str().to_owned()),
        tainted_days: Set(loss.tainted.to_json()),
        resolution: Set(None),
        evidence_ref: Set(None),
        resolved_by: Set(None),
        approved_by: Set(None),
        resolved_at: Set(None),
    };
    let row = source_loss::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("open source loss", e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| scope_failure("open source loss", e))?;
    loss_to_domain(row)
}

/// Every loss of `source` within `scope`: for tests and operator reads.
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn list_losses(
    runner: &impl DBRunner,
    scope: &AccessScope,
    source: SourceId,
) -> Result<Vec<SourceLoss>, DomainError> {
    source_loss::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(source_loss::Column::SourceId.eq(source.to_string())))
        .all(runner)
        .await
        .map_err(|e| scope_failure("list source losses", e))?
        .into_iter()
        .map(loss_to_domain)
        .collect()
}

#[derive(FromQueryResult)]
struct KindCountRow {
    kind: String,
    unresolved: i64,
}

/// The not-resolved losses of `source`, by kind.
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn count_unresolved(
    runner: &impl DBRunner,
    scope: &AccessScope,
    source: SourceId,
) -> Result<BTreeMap<LossKind, u64>, DomainError> {
    let rows = source_loss::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(source_loss::Column::SourceId.eq(source.to_string()))
                .add(source_loss::Column::State.ne(LossState::Resolved.as_str())),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(source_loss::Column::Kind)
                .column_as(source_loss::Column::LossId.count(), "unresolved")
                .group_by(source_loss::Column::Kind)
                .into_model::<KindCountRow>()
        })
        .await
        .map_err(|e| scope_failure("count source losses", e))?;
    rows.into_iter()
        .map(|row| {
            let kind =
                LossKind::parse(&row.kind).ok_or_else(|| corrupt(LOSS_TABLE, "kind", &row.kind))?;
            Ok((kind, u64::try_from(row.unresolved).unwrap_or(0)))
        })
        .collect()
}

fn loss_to_domain(row: source_loss::Model) -> Result<SourceLoss, DomainError> {
    let id = row.loss_id.to_string();
    Ok(SourceLoss {
        loss_id: row.loss_id,
        tenant_id: row.tenant_id,
        source_id: SourceId::parse(&row.source_id)
            .ok_or_else(|| corrupt(LOSS_TABLE, "source_id", &row.source_id))?,
        kind: LossKind::parse(&row.kind).ok_or_else(|| corrupt(LOSS_TABLE, "kind", &row.kind))?,
        state: LossState::parse(&row.state)
            .ok_or_else(|| corrupt(LOSS_TABLE, "state", &row.state))?,
        last_good_cursor: row
            .last_good_cursor
            .map(FeedCursor::parse)
            .transpose()
            .map_err(|_| corrupt(LOSS_TABLE, "last_good_cursor", &id))?,
        tainted: TaintedDays::from_json(&row.tainted_days)
            .map_err(|_| corrupt(LOSS_TABLE, "tainted_days", &id))?,
    })
}

// ---- ports -------------------------------------------------------------------------------------

/// The `SecureORM` implementation of the checkpoint, record and loss ports.
#[derive(Debug, Default, Clone, Copy)]
pub struct OrmUsageRepo;

#[async_trait::async_trait]
impl CheckpointRepo for OrmUsageRepo {
    async fn lock_usage(&self, tx: &DbTx<'_>) -> Result<Vec<Checkpoint>, DomainError> {
        lock_usage_checkpoints(tx).await
    }

    async fn lock(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
    ) -> Result<Option<Checkpoint>, DomainError> {
        lock_checkpoint(tx, source).await
    }

    async fn insert(&self, tx: &DbTx<'_>, checkpoint: &Checkpoint) -> Result<bool, DomainError> {
        insert_checkpoint(tx, checkpoint).await
    }

    async fn set_cursor(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
        cursor: Option<&FeedCursor>,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        set_cursor(tx, source, cursor, now).await
    }

    async fn set_state(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
        transition: Transition,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        set_source_state(tx, source, transition, now).await
    }

    async fn stamp_generation(&self, tx: &DbTx<'_>, generation: u64) -> Result<(), DomainError> {
        stamp_generation(tx, generation).await
    }
}

#[async_trait::async_trait]
impl UsageRecordRepo for OrmUsageRepo {
    async fn find(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
    ) -> Result<Option<StoredUsageRecord>, DomainError> {
        find_record(tx, scope, identity).await
    }

    async fn stored_digests(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<StoredDigest>, DomainError> {
        stored_digests(tx, scope, tenant_id, ids).await
    }

    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        record: &NewUsageRecord,
    ) -> Result<bool, DomainError> {
        insert_record(tx, scope, record).await
    }

    async fn natural_key_holder(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &NaturalKey,
        except: Uuid,
    ) -> Result<Option<Uuid>, DomainError> {
        natural_key_holder(tx, scope, key, except).await
    }

    async fn invalidation_naming(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        target: EntryIdentity,
        except: Uuid,
    ) -> Result<Option<Uuid>, DomainError> {
        invalidation_naming(tx, scope, target, except).await
    }

    async fn withdraw(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        target: EntryIdentity,
        from: ContributionState,
        by: Uuid,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        withdraw(tx, scope, target, from, by, at).await
    }

    async fn complete_retry(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        normalized: &NormalizedUsage,
        to: ContributionState,
    ) -> Result<bool, DomainError> {
        complete_retry(tx, scope, normalized, to).await
    }

    async fn reject_again(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
        reason: RejectReason,
    ) -> Result<bool, DomainError> {
        reject_again(tx, scope, identity, reason).await
    }

    async fn capture_rejected(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
        raw_sha256: &Sha256Hex,
    ) -> Result<bool, DomainError> {
        capture_rejected(tx, source, raw_sha256).await
    }

    async fn insert_capture_reject(
        &self,
        tx: &DbTx<'_>,
        reject: &CaptureReject,
    ) -> Result<bool, DomainError> {
        insert_capture_reject(tx, reject).await
    }
}

#[async_trait::async_trait]
impl SourceLossRepo for OrmUsageRepo {
    async fn open(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        loss: &NewSourceLoss,
    ) -> Result<SourceLoss, DomainError> {
        open_loss(tx, scope, loss).await
    }

    async fn count_unresolved(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        source: SourceId,
    ) -> Result<BTreeMap<LossKind, u64>, DomainError> {
        count_unresolved(tx, scope, source).await
    }
}
