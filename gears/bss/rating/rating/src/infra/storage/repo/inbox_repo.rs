//! `bss_rating__inbox`: insert-only except `state` (and `applied_at`, which moves with it).

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, FromQueryResult, QuerySelect, Set};
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;

use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::inbox::InboxRepo;
use crate::domain::inbox::{
    InboxEntry, InboxKey, InboxSource, InboxState, InboxTransition, InvalidInboxKey, Quarantine,
    QuarantineReason, ReceivedVia, StoredEntry,
};
use crate::infra::storage::entity::inbox::{ActiveModel, Column, Entity, Model};
use crate::infra::storage::{corrupt, scope_failure};

const TABLE: &str = "bss_rating__inbox";

/// Every stored entry of `key` within `scope`, payloads included: its primary entry and every
/// digest conflict. Unbounded, so it is for operator reads and tests; accepting a fact reads only
/// the primary's id ([`find_primary_id`]) and the one entry of its digest ([`find_by_id`]).
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn find_by_key(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &InboxKey,
) -> Result<Vec<InboxEntry>, DomainError> {
    Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key_condition(key))
        .all(runner)
        .await
        .map_err(|e| scope_failure("find inbox entries by key", e))?
        .into_iter()
        .map(to_domain)
        .collect()
}

#[derive(FromQueryResult)]
struct EntryIdRow {
    entry_id: Uuid,
}

/// The id of `key`'s primary entry within `scope`: the one entry of the key with no
/// `conflict_of`, held unique by `uq_bss_rating__inbox__primary`. Reads the id alone.
///
/// # Errors
///
/// A storage failure.
pub async fn find_primary_id(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &InboxKey,
) -> Result<Option<Uuid>, DomainError> {
    let rows = Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key_condition(key).add(Column::ConflictOf.is_null()))
        .project_all(runner, |q| {
            q.select_only()
                .column(Column::EntryId)
                .limit(1)
                .into_model::<EntryIdRow>()
        })
        .await
        .map_err(|e| scope_failure("find the primary inbox entry", e))?;
    Ok(rows.into_iter().next().map(|row| row.entry_id))
}

/// The entry `entry_id` if it lies within `scope`.
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn find_by_id(
    runner: &impl DBRunner,
    scope: &AccessScope,
    entry_id: Uuid,
) -> Result<Option<InboxEntry>, DomainError> {
    Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(Column::EntryId.eq(entry_id)))
        .one(runner)
        .await
        .map_err(|e| scope_failure("find inbox entry", e))?
        .map(to_domain)
        .transpose()
}

/// Inserts a new entry.
///
/// # Errors
///
/// [`DomainError::Concurrent`] when another writer stored the same key first; a scope refusal
/// when the entry's tenant lies outside `scope`; a storage failure.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    entry: &InboxEntry,
) -> Result<(), DomainError> {
    let key = entry.key();
    let active = ActiveModel {
        entry_id: Set(entry.entry_id()),
        tenant_id: Set(key.tenant_id()),
        source: Set(key.source().as_str().to_owned()),
        business_id: Set(key.business_id().to_owned()),
        version: Set(key.version()),
        payload_digest: Set(entry.payload_digest().as_str().to_owned()),
        payload: Set(entry.payload().clone()),
        received_via: Set(entry.received_via().as_str().to_owned()),
        state: Set(entry.state().as_str().to_owned()),
        quarantine_reason: Set(entry.quarantine().map(|q| q.reason().as_str().to_owned())),
        conflict_of: Set(entry.quarantine().and_then(Quarantine::conflict_of)),
        received_at: Set(entry.received_at()),
        applied_at: Set(entry.applied_at()),
    };
    Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("insert inbox entry", e))?
        .exec(runner)
        .await
        .map_err(|e| scope_failure("insert inbox entry", e))?;
    Ok(())
}

/// Makes `transition` on `entry_id` if the entry is still in its source state (a CAS): `true`
/// when this call made it, `false` when a concurrent writer had already moved the entry.
///
/// # Errors
///
/// A storage failure.
pub async fn transition(
    runner: &impl DBRunner,
    scope: &AccessScope,
    entry_id: Uuid,
    transition: InboxTransition,
) -> Result<bool, DomainError> {
    let from = transition.from();
    let result = Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(Column::State, Expr::value(transition.to().as_str()))
        .col_expr(Column::AppliedAt, Expr::value(transition.applied_at()))
        .filter(
            Condition::all()
                .add(Column::EntryId.eq(entry_id))
                .add(Column::State.eq(from.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| scope_failure("transition inbox entry", e))?;
    Ok(result.rows_affected == 1)
}

fn key_condition(key: &InboxKey) -> Condition {
    Condition::all()
        .add(Column::TenantId.eq(key.tenant_id()))
        .add(Column::Source.eq(key.source().as_str()))
        .add(Column::BusinessId.eq(key.business_id()))
        .add(Column::Version.eq(key.version()))
}

fn to_domain(row: Model) -> Result<InboxEntry, DomainError> {
    let source =
        InboxSource::parse(&row.source).ok_or_else(|| corrupt(TABLE, "source", &row.source))?;
    // The key names the part out of bounds, so the corrupt column is reported as itself.
    let key = InboxKey::new(row.tenant_id, source, row.business_id.as_str(), row.version).map_err(
        |invalid| match invalid {
            InvalidInboxKey::BusinessId => corrupt(TABLE, "business_id", &row.business_id),
            InvalidInboxKey::Version => corrupt(TABLE, "version", &row.version.to_string()),
        },
    )?;
    let payload_digest = Sha256Hex::parse(&row.payload_digest)
        .map_err(|_| corrupt(TABLE, "payload_digest", &row.payload_digest))?;
    let received_via = ReceivedVia::parse(&row.received_via)
        .ok_or_else(|| corrupt(TABLE, "received_via", &row.received_via))?;
    let state = InboxState::parse(&row.state).ok_or_else(|| corrupt(TABLE, "state", &row.state))?;
    let quarantine = row
        .quarantine_reason
        .as_deref()
        .map(|r| {
            let reason =
                QuarantineReason::parse(r).ok_or_else(|| corrupt(TABLE, "quarantine_reason", r))?;
            Quarantine::from_parts(reason, row.conflict_of).ok_or_else(|| {
                corrupt(
                    TABLE,
                    "conflict_of",
                    &format!("{:?} with {r}", row.conflict_of),
                )
            })
        })
        .transpose()?;
    if quarantine.is_none() && row.conflict_of.is_some() {
        return Err(corrupt(
            TABLE,
            "conflict_of",
            "set without a quarantine reason",
        ));
    }
    let entry_id = row.entry_id;
    InboxEntry::restore(StoredEntry {
        entry_id,
        key,
        payload_digest,
        payload: row.payload,
        received_via,
        state,
        quarantine,
        received_at: row.received_at,
        applied_at: row.applied_at,
    })
    .map_err(|column| corrupt(TABLE, column, &entry_id.to_string()))
}

/// The `SecureORM` implementation of the domain's [`InboxRepo`] port.
#[derive(Debug, Default, Clone, Copy)]
pub struct OrmInboxRepo;

#[async_trait::async_trait]
impl InboxRepo for OrmInboxRepo {
    async fn find_primary_id(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &InboxKey,
    ) -> Result<Option<Uuid>, DomainError> {
        find_primary_id(tx, scope, key).await
    }

    async fn find_by_id(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entry_id: Uuid,
    ) -> Result<Option<InboxEntry>, DomainError> {
        find_by_id(tx, scope, entry_id).await
    }

    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entry: &InboxEntry,
    ) -> Result<(), DomainError> {
        insert(tx, scope, entry).await
    }

    async fn transition(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entry_id: Uuid,
        moved: InboxTransition,
    ) -> Result<bool, DomainError> {
        transition(tx, scope, entry_id, moved).await
    }
}
