//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-store:p1
//! Quota intake persistence (P-D-268, P-D-269, P-D-274): adapter targets, verified snapshots and
//! candidates, every read and write scoped by the caller's `AccessScope` and the tenant.
//!
//! A target and a snapshot are inserted once and never written again: the tables' triggers refuse
//! every `UPDATE` and `DELETE`, and this module has no statement that tries. A candidate's content
//! is inserted once; only its review columns move, one transition per version the caller read
//! ([`review_candidate`]). Revisions compare as numbers: by length, then by text.
//!
//! # Rows that do not decode
//!
//! Every quota JSON column holds an object, and every write here checks it. A stored row that does
//! not read as its stored shape (a closed token outside its set, a count or version outside its
//! range, a JSON column that is not an object) is a [`RepoError::CorruptRow`] naming its table,
//! tenant and id: never a missing row and never a row skipped from a page (P-D-274). JSON text that
//! does not parse at all, which only an out-of-band `SQLite` write can store (Postgres `jsonb`
//! refuses it at the write), fails inside the driver: the whole statement fails as a driver error,
//! which is still a 500 and never a missing row, but names no row.
use super::{SkuListError, driver_failure};
use crate::domain::quota::{
    CandidateFilter, CandidateOrigin, CandidateState, NewCandidate, NewQuotaTarget, QuotaCandidate,
    QuotaTarget, ReviewWrite, StoredSnapshot,
};
use crate::infra::storage::{
    RepoError, RepoRefusal,
    entity::{quota_adapter_target, quota_candidate, quota_snapshot},
};
use bss_products_sdk::quota::catalog::QuotaVersion;
use sea_orm::sea_query::{Expr, ExprTrait, Func};
use sea_orm::{ColumnTrait, Condition, EntityTrait, FromQueryResult, Order, QuerySelect, Set};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, LimitCfg, ODataFieldMapping, PaginateOdataTryError, paginate_odata_try,
};
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
    secure_insert_many,
};
use toolkit_odata::filter::{FieldKind, FilterField, FilterOp, ODataValue};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use uuid::Uuid;

/// The quota lists' page: 50 by default, clamped at 200, as the SKU list (P-D-210).
pub const QUOTA_PAGE: LimitCfg = LimitCfg {
    default: 50,
    max: 200,
};

const SNAPSHOT_TABLE: &str = "products_quota_snapshot";
const CANDIDATE_TABLE: &str = "products_quota_candidate";

/// A stored quota row that does not decode, named by its table, tenant and id.
pub(super) fn corrupt(
    table: &str,
    tenant: Uuid,
    id: impl std::fmt::Display,
    why: &str,
) -> RepoError {
    RepoError::CorruptRow(format!(
        "a stored quota row does not read: {table}, tenant {tenant}, id {id}: {why}"
    ))
}

/// A stored JSON column, which holds an object.
pub(super) fn stored_object(
    value: Value,
    table: &str,
    tenant: Uuid,
    id: impl std::fmt::Display,
    column: &str,
) -> Result<Value, RepoError> {
    if value.is_object() {
        Ok(value)
    } else {
        Err(corrupt(
            table,
            tenant,
            id,
            &format!("{column} is not a JSON object"),
        ))
    }
}

/// A JSON value about to be stored, which must be an object.
pub(super) fn object_to_store(
    value: &Value,
    table: &str,
    column: &str,
) -> Result<Value, RepoError> {
    if value.is_object() {
        Ok(value.clone())
    } else {
        Err(RepoError::Db(format!(
            "{table}.{column} must be a JSON object"
        )))
    }
}

/// A stored version column as the SDK's version: from 1, within 64 bits.
pub(super) fn stored_version(
    value: i64,
    table: &str,
    tenant: Uuid,
    id: impl std::fmt::Display,
    column: &str,
) -> Result<QuotaVersion, RepoError> {
    u64::try_from(value)
        .ok()
        .and_then(|v| QuotaVersion::new(v).ok())
        .ok_or_else(|| {
            corrupt(
                table,
                tenant,
                id,
                &format!("{column} {value} is not a version"),
            )
        })
}

/// A version about to be stored: the column is `bigint`, so a version above `i64::MAX` is refused,
/// never wrapped.
pub(super) fn version_to_store(version: QuotaVersion, what: &str) -> Result<i64, RepoError> {
    i64::try_from(version.get())
        .map_err(|_| RepoError::Db(format!("{what} {version} is beyond the stored range")))
}

/// The pager's failure as the list's error; a driver failure keeps its message for the retry
/// classifier.
pub(super) fn list_error(context: &str, e: PaginateOdataTryError<RepoError>) -> SkuListError {
    match e {
        PaginateOdataTryError::OData(toolkit_odata::Error::Db(message)) => {
            SkuListError::Repo(RepoError::Driver {
                context: context.to_owned(),
                source: sea_orm::DbErr::Custom(message),
            })
        }
        PaginateOdataTryError::OData(other) => SkuListError::Query(other),
        PaginateOdataTryError::MapError(e) => SkuListError::Repo(e),
    }
}

fn unique_refusal(context: &str, e: ScopeError, names: &[&str], refusal: RepoRefusal) -> RepoError {
    if e.is_unique_violation() {
        let text = e.to_string();
        if names.iter().any(|name| text.contains(name)) {
            return RepoError::Refused(refusal);
        }
    }
    driver_failure(context.to_owned(), e)
}

fn target_of(m: quota_adapter_target::Model) -> QuotaTarget {
    QuotaTarget {
        id: m.id,
        tenant_id: m.tenant_id,
        plugin_instance_id: m.plugin_instance_id,
        target_key: m.target_key,
        name: m.name,
        created_by: m.created_by,
        created_at: m.created_at,
    }
}

fn snapshot_of(m: quota_snapshot::Model) -> Result<StoredSnapshot, RepoError> {
    let (t, id) = (m.tenant_id, m.id);
    let item_count = u64::try_from(m.item_count).map_err(|_| {
        corrupt(
            SNAPSHOT_TABLE,
            t,
            id,
            &format!("item_count {}", m.item_count),
        )
    })?;
    Ok(StoredSnapshot {
        id,
        target_id: m.target_id,
        snapshot_id: m.snapshot_id,
        source_revision: m.source_revision,
        observed_at: m.observed_at,
        item_count,
        content_digest: m.content_digest,
        snapshot_json: stored_object(m.snapshot_json, SNAPSHOT_TABLE, t, id, "snapshot_json")?,
        evidence_json: stored_object(m.evidence_json, SNAPSHOT_TABLE, t, id, "evidence_json")?,
        review_json: stored_object(m.review_json, SNAPSHOT_TABLE, t, id, "review_json")?,
        created_by: m.created_by,
        created_at: m.created_at,
    })
}

fn candidate_of(m: quota_candidate::Model) -> Result<QuotaCandidate, RepoError> {
    let (t, id) = (m.tenant_id, m.id);
    let origin = CandidateOrigin::parse(&m.origin)
        .ok_or_else(|| corrupt(CANDIDATE_TABLE, t, id, &format!("origin {}", m.origin)))?;
    let state = CandidateState::parse(&m.state)
        .ok_or_else(|| corrupt(CANDIDATE_TABLE, t, id, &format!("state {}", m.state)))?;
    let optional = |value: Option<Value>, column: &str| {
        value
            .map(|v| stored_object(v, CANDIDATE_TABLE, t, id, column))
            .transpose()
    };
    Ok(QuotaCandidate {
        id,
        target_id: m.target_id,
        origin,
        snapshot_row_id: m.snapshot_row_id,
        record_id: m.record_id,
        capability_key: m.capability_key,
        record_json: stored_object(m.record_json, CANDIDATE_TABLE, t, id, "record_json")?,
        record_digest: m.record_digest,
        evidence_json: stored_object(m.evidence_json, CANDIDATE_TABLE, t, id, "evidence_json")?,
        state,
        version: m.version,
        proposal_json: optional(m.proposal_json, "proposal_json")?,
        proposal_evidence_json: optional(m.proposal_evidence_json, "proposal_evidence_json")?,
        created_by: m.created_by,
        created_at: m.created_at,
        submitted_by: m.submitted_by,
        submitted_at: m.submitted_at,
        submit_note: m.submit_note,
        decided_by: m.decided_by,
        decided_at: m.decided_at,
        decided_note: m.decided_note,
        definition_id: m.definition_id,
        definition_version: m
            .definition_version
            .map(|v| stored_version(v, CANDIDATE_TABLE, t, id, "definition_version"))
            .transpose()?,
    })
}

/// Insert a target, its `id` fresh.
/// # Errors
/// `QUOTA_TARGET_TAKEN` when the tenant has bound this plugin instance and target key; scoped
/// storage failures.
pub async fn insert_target(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    new: NewQuotaTarget,
    created_by: Uuid,
    now: OffsetDateTime,
) -> Result<QuotaTarget, RepoError> {
    let model = quota_adapter_target::ActiveModel {
        tenant_id: Set(tenant_id),
        id: Set(Uuid::now_v7()),
        plugin_instance_id: Set(new.plugin_instance_id),
        target_key: Set(new.target_key),
        name: Set(new.name),
        created_by: Set(created_by),
        created_at: Set(now),
    };
    quota_adapter_target::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("quota target scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map(target_of)
        .map_err(|e| {
            unique_refusal(
                "insert quota target",
                e,
                &[
                    "uq_products_quota_adapter_target_binding",
                    "products_quota_adapter_target.target_key",
                ],
                RepoRefusal::QuotaTargetTaken,
            )
        })
}

/// The tenant's target `id`, within the scope.
/// # Errors
/// Scoped storage failures.
pub async fn find_target(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Option<QuotaTarget>, RepoError> {
    quota_adapter_target::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(quota_adapter_target::Column::TenantId.eq(tenant_id))
                .add(quota_adapter_target::Column::Id.eq(id)),
        )
        .one(runner)
        .await
        .map(|m| m.map(target_of))
        .map_err(|e| driver_failure("find quota target".into(), e))
}

/// The target list's pager fields: `name`, with `id` as the tie-break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetListField {
    Name,
    Id,
}
impl FilterField for TargetListField {
    const FIELDS: &'static [Self] = &[Self::Name, Self::Id];
    fn name(&self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Id => "id",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::Name => FieldKind::String,
            Self::Id => FieldKind::Uuid,
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
/// How the pager reads the target row for each field.
pub struct TargetListMapping;
impl FieldToColumn<TargetListField> for TargetListMapping {
    type Column = quota_adapter_target::Column;
    fn map_field(field: TargetListField) -> quota_adapter_target::Column {
        match field {
            TargetListField::Name => quota_adapter_target::Column::Name,
            TargetListField::Id => quota_adapter_target::Column::Id,
        }
    }
    fn map_value(
        _field: TargetListField,
        _op: FilterOp,
        value: &ODataValue,
    ) -> Result<ODataValue, String> {
        Ok(value.clone())
    }
}
impl ODataFieldMapping<TargetListField> for TargetListMapping {
    type Entity = quota_adapter_target::Entity;
    fn extract_cursor_value(
        model: &quota_adapter_target::Model,
        field: TargetListField,
    ) -> sea_orm::Value {
        match field {
            TargetListField::Name => sea_orm::Value::String(Some(model.name.clone())),
            TargetListField::Id => sea_orm::Value::Uuid(Some(model.id)),
        }
    }
}

/// One page of the tenant's targets, by name (tie-break id); `limit` defaults to 50 and is clamped
/// at 200, and a cursor continues the walk. ONE statement, through
/// `idx_products_quota_adapter_target_name`.
/// # Errors
/// [`SkuListError::Query`] for a cursor the pager refuses; [`SkuListError::Repo`] for storage.
pub async fn list_targets(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    query: &ODataQuery,
) -> Result<Page<QuotaTarget>, SkuListError> {
    let mut query = query.clone();
    if query.cursor.is_none() && query.order.0.is_empty() {
        query.order = ODataOrderBy(vec![OrderKey {
            field: TargetListField::Name.name().to_owned(),
            dir: SortDir::Asc,
        }]);
    }
    let select = quota_adapter_target::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(quota_adapter_target::Column::TenantId.eq(tenant_id)));
    paginate_odata_try::<
        TargetListField,
        TargetListMapping,
        quota_adapter_target::Entity,
        QuotaTarget,
        _,
        RepoError,
        _,
    >(
        select,
        runner,
        &query,
        (TargetListField::Id.name(), SortDir::Asc),
        QUOTA_PAGE,
        |m| Ok(target_of(m)),
    )
    .await
    .map_err(|e| list_error("list quota targets", e))
}

fn snapshot_key(tenant_id: Uuid, target_id: Uuid) -> Condition {
    Condition::all()
        .add(quota_snapshot::Column::TenantId.eq(tenant_id))
        .add(quota_snapshot::Column::TargetId.eq(target_id))
}

/// The target's snapshot at `revision`, within the scope.
/// # Errors
/// Scoped storage failures; a stored row that does not decode.
pub async fn snapshot_at(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    target_id: Uuid,
    revision: &str,
) -> Result<Option<StoredSnapshot>, RepoError> {
    quota_snapshot::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            snapshot_key(tenant_id, target_id)
                .add(quota_snapshot::Column::SourceRevision.eq(revision)),
        )
        .one(runner)
        .await
        .map_err(|e| driver_failure("quota snapshot at revision".into(), e))?
        .map(snapshot_of)
        .transpose()
}

#[derive(Debug, FromQueryResult)]
struct Revision {
    source_revision: String,
}

/// The target's highest stored revision, compared as a number: the longest canonical decimal,
/// then the greatest text. ONE statement, which reads the revision column only: never a snapshot's
/// records.
/// # Errors
/// Scoped storage failures.
pub async fn highest_revision(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    target_id: Uuid,
) -> Result<Option<String>, RepoError> {
    let rows: Vec<Revision> = quota_snapshot::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(snapshot_key(tenant_id, target_id))
        .order_by(
            Expr::expr(Func::char_length(Expr::col(
                quota_snapshot::Column::SourceRevision,
            ))),
            Order::Desc,
        )
        .order_by(quota_snapshot::Column::SourceRevision, Order::Desc)
        .limit(1)
        .project_all(runner, |query| {
            query
                .select_only()
                .column(quota_snapshot::Column::SourceRevision)
                .into_model::<Revision>()
        })
        .await
        .map_err(|e| driver_failure("highest quota revision".into(), e))?;
    Ok(rows.into_iter().next().map(|row| row.source_revision))
}

/// Insert a verified snapshot as given.
/// # Errors
/// `QUOTA_SOURCE_CONFLICT` (`QuotaSnapshotTaken`) when the target has a snapshot at this revision;
/// a JSON column that is not an object; scoped storage failures.
pub async fn insert_snapshot(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    snapshot: &StoredSnapshot,
) -> Result<(), RepoError> {
    let item_count = i64::try_from(snapshot.item_count).map_err(|_| {
        RepoError::Db(format!(
            "{SNAPSHOT_TABLE}.item_count {} is beyond the stored range",
            snapshot.item_count
        ))
    })?;
    let model = quota_snapshot::ActiveModel {
        tenant_id: Set(tenant_id),
        id: Set(snapshot.id),
        target_id: Set(snapshot.target_id),
        snapshot_id: Set(snapshot.snapshot_id),
        source_revision: Set(snapshot.source_revision.clone()),
        observed_at: Set(snapshot.observed_at),
        item_count: Set(item_count),
        content_digest: Set(snapshot.content_digest.clone()),
        snapshot_json: Set(object_to_store(
            &snapshot.snapshot_json,
            SNAPSHOT_TABLE,
            "snapshot_json",
        )?),
        evidence_json: Set(object_to_store(
            &snapshot.evidence_json,
            SNAPSHOT_TABLE,
            "evidence_json",
        )?),
        review_json: Set(object_to_store(
            &snapshot.review_json,
            SNAPSHOT_TABLE,
            "review_json",
        )?),
        created_by: Set(snapshot.created_by),
        created_at: Set(snapshot.created_at),
    };
    quota_snapshot::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("quota snapshot scope".into(), e))?
        .exec(runner)
        .await
        .map(|_| ())
        .map_err(|e| {
            unique_refusal(
                "insert quota snapshot",
                e,
                &[
                    "uq_products_quota_snapshot_revision",
                    "products_quota_snapshot.source_revision",
                ],
                RepoRefusal::QuotaSnapshotTaken,
            )
        })
}

/// Insert candidates, each `staged` at version 1 with a fresh `id`; answers them as stored, in the
/// order given. The rows go in as few statements as the backend's bind limit allows.
/// # Errors
/// `QUOTA_SOURCE_CONFLICT` (`QuotaManualRecordTaken`) when the target has a manual candidate for a
/// record id; a JSON column that is not an object; scoped storage failures.
pub async fn insert_candidates(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    new: &[NewCandidate],
) -> Result<Vec<QuotaCandidate>, RepoError> {
    let mut models = Vec::with_capacity(new.len());
    let mut stored = Vec::with_capacity(new.len());
    for candidate in new {
        let id = Uuid::now_v7();
        let record_json = object_to_store(&candidate.record_json, CANDIDATE_TABLE, "record_json")?;
        let evidence_json =
            object_to_store(&candidate.evidence_json, CANDIDATE_TABLE, "evidence_json")?;
        models.push(quota_candidate::ActiveModel {
            tenant_id: Set(tenant_id),
            id: Set(id),
            target_id: Set(candidate.target_id),
            origin: Set(candidate.origin.as_str().to_owned()),
            snapshot_row_id: Set(candidate.snapshot_row_id),
            record_id: Set(candidate.record_id),
            capability_key: Set(candidate.capability_key.clone()),
            record_json: Set(record_json.clone()),
            record_digest: Set(candidate.record_digest.clone()),
            evidence_json: Set(evidence_json.clone()),
            state: Set(CandidateState::Staged.as_str().to_owned()),
            version: Set(1),
            proposal_json: Set(None),
            proposal_evidence_json: Set(None),
            created_by: Set(candidate.created_by),
            created_at: Set(candidate.created_at),
            submitted_by: Set(None),
            submitted_at: Set(None),
            submit_note: Set(None),
            decided_by: Set(None),
            decided_at: Set(None),
            decided_note: Set(None),
            definition_id: Set(None),
            definition_version: Set(None),
        });
        stored.push(QuotaCandidate {
            id,
            target_id: candidate.target_id,
            origin: candidate.origin,
            snapshot_row_id: candidate.snapshot_row_id,
            record_id: candidate.record_id,
            capability_key: candidate.capability_key.clone(),
            record_json,
            record_digest: candidate.record_digest.clone(),
            evidence_json,
            state: CandidateState::Staged,
            version: 1,
            proposal_json: None,
            proposal_evidence_json: None,
            created_by: candidate.created_by,
            created_at: candidate.created_at,
            submitted_by: None,
            submitted_at: None,
            submit_note: None,
            decided_by: None,
            decided_at: None,
            decided_note: None,
            definition_id: None,
            definition_version: None,
        });
    }
    secure_insert_many::<quota_candidate::Entity>(models, scope, runner)
        .await
        .map_err(|e| {
            unique_refusal(
                "insert quota candidates",
                e,
                &[
                    "uq_products_quota_candidate_manual_record",
                    "products_quota_candidate.record_id",
                ],
                RepoRefusal::QuotaManualRecordTaken,
            )
        })?;
    Ok(stored)
}

fn candidate_key(tenant_id: Uuid) -> Condition {
    Condition::all().add(quota_candidate::Column::TenantId.eq(tenant_id))
}

/// Every candidate of the snapshot row, by capability key.
/// # Errors
/// Scoped storage failures; a stored row that does not decode.
pub async fn candidates_of_snapshot(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    snapshot_row_id: Uuid,
) -> Result<Vec<QuotaCandidate>, RepoError> {
    quota_candidate::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            candidate_key(tenant_id)
                .add(quota_candidate::Column::SnapshotRowId.eq(snapshot_row_id)),
        )
        .order_by(quota_candidate::Column::CapabilityKey, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("quota candidates of snapshot".into(), e))?
        .into_iter()
        .map(candidate_of)
        .collect()
}

/// The target's manual candidate for `record_id`.
/// # Errors
/// Scoped storage failures; a stored row that does not decode.
pub async fn manual_candidate(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    target_id: Uuid,
    record_id: Uuid,
) -> Result<Option<QuotaCandidate>, RepoError> {
    quota_candidate::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            candidate_key(tenant_id)
                .add(quota_candidate::Column::TargetId.eq(target_id))
                .add(quota_candidate::Column::RecordId.eq(record_id))
                .add(quota_candidate::Column::Origin.eq(CandidateOrigin::Manual.as_str())),
        )
        .one(runner)
        .await
        .map_err(|e| driver_failure("manual quota candidate".into(), e))?
        .map(candidate_of)
        .transpose()
}

/// The tenant's candidate `id`, within the scope.
/// # Errors
/// Scoped storage failures; a stored row that does not decode.
pub async fn find_candidate(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Option<QuotaCandidate>, RepoError> {
    quota_candidate::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(candidate_key(tenant_id).add(quota_candidate::Column::Id.eq(id)))
        .one(runner)
        .await
        .map_err(|e| driver_failure("find quota candidate".into(), e))?
        .map(candidate_of)
        .transpose()
}

/// The candidate list's pager fields: `created_at`, with `id` as the tie-break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateListField {
    CreatedAt,
    Id,
}
impl FilterField for CandidateListField {
    const FIELDS: &'static [Self] = &[Self::CreatedAt, Self::Id];
    fn name(&self) -> &'static str {
        match self {
            Self::CreatedAt => "created_at",
            Self::Id => "id",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::CreatedAt => FieldKind::DateTimeUtc,
            Self::Id => FieldKind::Uuid,
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
/// How the pager reads the candidate row for each field.
pub struct CandidateListMapping;
impl FieldToColumn<CandidateListField> for CandidateListMapping {
    type Column = quota_candidate::Column;
    fn map_field(field: CandidateListField) -> quota_candidate::Column {
        match field {
            CandidateListField::CreatedAt => quota_candidate::Column::CreatedAt,
            CandidateListField::Id => quota_candidate::Column::Id,
        }
    }
    fn map_value(
        _field: CandidateListField,
        _op: FilterOp,
        value: &ODataValue,
    ) -> Result<ODataValue, String> {
        Ok(value.clone())
    }
}
impl ODataFieldMapping<CandidateListField> for CandidateListMapping {
    type Entity = quota_candidate::Entity;
    fn extract_cursor_value(
        model: &quota_candidate::Model,
        field: CandidateListField,
    ) -> sea_orm::Value {
        match field {
            CandidateListField::CreatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(model.created_at))
            }
            CandidateListField::Id => sea_orm::Value::Uuid(Some(model.id)),
        }
    }
}

/// One page of the tenant's candidates that `filter` keeps, newest first (tie-break id, newest
/// first too); `limit` defaults to 50 and is clamped at 200, and a cursor continues the walk. ONE
/// statement, through the candidate index the narrowing leads with.
/// # Errors
/// [`SkuListError::Query`] for a cursor the pager refuses; [`SkuListError::Repo`] for storage and
/// a stored row that does not decode.
pub async fn list_candidates(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    filter: &CandidateFilter,
    query: &ODataQuery,
) -> Result<Page<QuotaCandidate>, SkuListError> {
    let mut query = query.clone();
    if query.cursor.is_none() && query.order.0.is_empty() {
        query.order = ODataOrderBy(vec![OrderKey {
            field: CandidateListField::CreatedAt.name().to_owned(),
            dir: SortDir::Desc,
        }]);
    }
    let tie = query.order.0.first().map_or(SortDir::Desc, |key| key.dir);
    let mut condition = candidate_key(tenant_id);
    if let Some(target_id) = filter.target_id {
        condition = condition.add(quota_candidate::Column::TargetId.eq(target_id));
    }
    if let Some(state) = filter.state {
        condition = condition.add(quota_candidate::Column::State.eq(state.as_str()));
    }
    let select = quota_candidate::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(condition);
    paginate_odata_try::<
        CandidateListField,
        CandidateListMapping,
        quota_candidate::Entity,
        QuotaCandidate,
        _,
        RepoError,
        _,
    >(
        select,
        runner,
        &query,
        (CandidateListField::Id.name(), tie),
        QUOTA_PAGE,
        candidate_of,
    )
    .await
    .map_err(|e| list_error("list quota candidates", e))
}

/// Move the candidate's review columns once, from the state `write` leaves and only at the version
/// the caller read: `UPDATE … WHERE tenant_id = :t AND id = :id AND version = :seen AND state =
/// :from`, the version moving by one in the same statement. `true` when the row moved; `false`
/// when another write moved it first or it is in another state, which the caller tells apart by
/// reading it again (P-D-269).
/// # Errors
/// A JSON column that is not an object; a version beyond the stored range; scoped storage
/// failures.
pub async fn review_candidate(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    seen_version: i64,
    write: ReviewWrite,
) -> Result<bool, RepoError> {
    use quota_candidate::Column as C;
    let from = write.state_before();
    let to = write.state_after();
    let mut update = quota_candidate::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(C::State, Expr::value(to.as_str()))
        .col_expr(C::Version, Expr::col(C::Version).add(1_i64));
    update = match write {
        ReviewWrite::Submit {
            proposal,
            evidence,
            by,
            at,
            note,
        } => update
            .col_expr(
                C::ProposalJson,
                Expr::value(object_to_store(
                    &proposal,
                    CANDIDATE_TABLE,
                    "proposal_json",
                )?),
            )
            .col_expr(
                C::ProposalEvidenceJson,
                Expr::value(object_to_store(
                    &evidence,
                    CANDIDATE_TABLE,
                    "proposal_evidence_json",
                )?),
            )
            .col_expr(C::SubmittedBy, Expr::value(by))
            .col_expr(C::SubmittedAt, Expr::value(at))
            .col_expr(C::SubmitNote, Expr::value(note)),
        ReviewWrite::Approve {
            by,
            at,
            note,
            definition_id,
            definition_version,
        } => update
            .col_expr(C::DecidedBy, Expr::value(by))
            .col_expr(C::DecidedAt, Expr::value(at))
            .col_expr(C::DecidedNote, Expr::value(note))
            .col_expr(C::DefinitionId, Expr::value(definition_id))
            .col_expr(
                C::DefinitionVersion,
                Expr::value(version_to_store(definition_version, "definition_version")?),
            ),
        ReviewWrite::Reject { by, at, note } => update
            .col_expr(C::DecidedBy, Expr::value(by))
            .col_expr(C::DecidedAt, Expr::value(at))
            .col_expr(C::DecidedNote, Expr::value(note)),
    };
    let result = update
        .filter(
            candidate_key(tenant_id)
                .add(C::Id.eq(id))
                .add(C::Version.eq(seen_version))
                .add(C::State.eq(from.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("review quota candidate".into(), e))?;
    Ok(result.rows_affected == 1)
}

#[cfg(test)]
#[path = "quota_intake_repo_tests.rs"]
pub(crate) mod quota_intake_repo_tests;
