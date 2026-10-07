//! Approval persistence over the gear's real transaction runner.
use super::driver_failure;
use crate::domain::approvals::{ApprovalKind, DEFAULT_QUORUM};
use crate::infra::storage::{
    RepoError,
    entity::{approval_decision, approval_policy, approval_unit, approval_unit_item},
};
use bss_approval::{ApprovalError, Decision, ItemRef, Policy, Store, Unit, UnitState, Verdict};
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, Set};
use std::collections::BTreeMap;
use time::OffsetDateTime;
use toolkit_db::DbTx;
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, LimitCfg, ODataFieldMapping, PaginateOdataTryError, paginate_odata_try,
};
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt,
    SecureOnConflict, SecureUpdateExt,
};
use toolkit_odata::filter::{FieldKind, FilterField};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use uuid::Uuid;

/// The tenant-scoped SQL implementation used inside each approval transaction.
#[derive(Clone)]
pub struct ProductsApprovalStore {
    pub scope: AccessScope,
    pub tenant_id: Uuid,
}
/// Preserve real driver errors so transaction retries can classify contention.
fn store_err(context: &str, e: ScopeError) -> ApprovalError {
    match e {
        ScopeError::Db(e) => ApprovalError::Db(e),
        other => ApprovalError::Store(format!("{context}: {other}")),
    }
}
fn unit_key(tenant: Uuid, id: Uuid) -> Condition {
    Condition::all()
        .add(approval_unit::Column::TenantId.eq(tenant))
        .add(approval_unit::Column::Id.eq(id))
}
fn item_key(tenant: Uuid, id: Uuid) -> Condition {
    Condition::all()
        .add(approval_unit_item::Column::TenantId.eq(tenant))
        .add(approval_unit_item::Column::UnitId.eq(id))
}
fn decision_key(tenant: Uuid, id: Uuid) -> Condition {
    Condition::all()
        .add(approval_decision::Column::TenantId.eq(tenant))
        .add(approval_decision::Column::UnitId.eq(id))
}
/// A stored unit read back. Its kind is one products records and its state one of the unit's:
/// a row outside either set is a corrupt row, refused by every reader alike (the list, the card,
/// the receipts, the votes; the counts judge the same sets).
fn unit_from_model(m: approval_unit::Model) -> Result<Unit, ApprovalError> {
    if ApprovalKind::parse(&m.kind).is_none() {
        return Err(ApprovalError::Store(format!(
            "approval unit {} has unknown kind {}",
            m.id, m.kind
        )));
    }
    Ok(Unit {
        id: m.id,
        tenant_id: m.tenant_id,
        kind: m.kind,
        ref_type: m.ref_type,
        ref_id: m.ref_id,
        state: UnitState::parse(&m.state)
            .ok_or_else(|| ApprovalError::Store(format!("invalid unit state {}", m.state)))?,
        common_effective_date: m.common_effective_date,
        quorum_required: u32::try_from(m.quorum_required)
            .map_err(|e| ApprovalError::Store(e.to_string()))?,
        generation: m.generation,
        submitted_by: m.submitted_by,
        submitted_at: m.submitted_at,
        submit_note: m.submit_note,
        decided_at: m.decided_at,
        decided_note: m.decided_note,
        snapshot: m.snapshot,
        snapshot_hash: m.snapshot_hash,
        version: m.version,
    })
}
fn decision_from_model(m: approval_decision::Model) -> Result<Decision, ApprovalError> {
    Ok(Decision {
        unit_id: m.unit_id,
        actor: m.actor,
        generation: m.generation,
        verdict: Verdict::parse(&m.decision)
            .ok_or_else(|| ApprovalError::Store(format!("invalid decision {}", m.decision)))?,
        note: m.note,
        at: m.at,
        stale: m.stale,
    })
}
impl ProductsApprovalStore {
    async fn insert_items(
        &self,
        runner: &impl DBRunner,
        id: Uuid,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        for i in items {
            let m = approval_unit_item::ActiveModel {
                unit_id: Set(id),
                tenant_id: Set(self.tenant_id),
                item_type: Set(i.item_type.clone()),
                item_id: Set(i.item_id),
                created_by: Set(i.created_by),
                before_json: Set(i.before.clone()),
                after_json: Set(i.after.clone()),
            };
            approval_unit_item::Entity::insert(m.clone())
                .secure()
                .scope_with_model(&self.scope, &m)
                .map_err(|e| store_err("item scope", e))?
                .exec(runner)
                .await
                .map_err(|e| store_err("insert item", e))?;
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl<'a> Store<DbTx<'a>> for ProductsApprovalStore {
    async fn insert_unit(
        &self,
        runner: &DbTx<'a>,
        unit: &Unit,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        if unit.tenant_id != self.tenant_id {
            return Err(ApprovalError::Store(
                "unit tenant differs from store tenant".into(),
            ));
        }
        let m = approval_unit::ActiveModel {
            id: Set(unit.id),
            tenant_id: Set(self.tenant_id),
            kind: Set(unit.kind.clone()),
            ref_type: Set(unit.ref_type.clone()),
            ref_id: Set(unit.ref_id),
            state: Set(unit.state.as_str().into()),
            common_effective_date: Set(unit.common_effective_date),
            quorum_required: Set(i32::try_from(unit.quorum_required)
                .map_err(|e| ApprovalError::Store(e.to_string()))?),
            generation: Set(unit.generation),
            submitted_by: Set(unit.submitted_by),
            submitted_at: Set(unit.submitted_at),
            decided_at: Set(unit.decided_at),
            decided_note: Set(unit.decided_note.clone()),
            snapshot: Set(unit.snapshot.clone()),
            snapshot_hash: Set(unit.snapshot_hash.clone()),
            version: Set(unit.version),
            submit_note: Set(unit.submit_note.clone()),
        };
        approval_unit::Entity::insert(m.clone())
            .secure()
            .scope_with_model(&self.scope, &m)
            .map_err(|e| store_err("unit scope", e))?
            .exec(runner)
            .await
            .map_err(|e| store_err("insert unit", e))?;
        self.insert_items(runner, unit.id, items).await
    }
    async fn unit(&self, runner: &DbTx<'a>, id: Uuid) -> Result<Option<Unit>, ApprovalError> {
        find_unit(runner, &self.scope, self.tenant_id, id).await
    }
    async fn bump_version(
        &self,
        runner: &DbTx<'a>,
        id: Uuid,
        expected: i64,
    ) -> Result<bool, ApprovalError> {
        let r = approval_unit::Entity::update_many()
            .secure()
            .scope_with(&self.scope)
            .col_expr(
                approval_unit::Column::Version,
                Expr::col(approval_unit::Column::Version).add(1_i64),
            )
            .filter(unit_key(self.tenant_id, id).add(approval_unit::Column::Version.eq(expected)))
            .exec(runner)
            .await
            .map_err(|e| store_err("bump version", e))?;
        Ok(r.rows_affected == 1)
    }
    async fn items(&self, runner: &DbTx<'a>, id: Uuid) -> Result<Vec<ItemRef>, ApprovalError> {
        Ok(approval_unit_item::Entity::find()
            .secure()
            .scope_with(&self.scope)
            .filter(item_key(self.tenant_id, id))
            .order_by(approval_unit_item::Column::ItemType, Order::Asc)
            .order_by(approval_unit_item::Column::ItemId, Order::Asc)
            .all(runner)
            .await
            .map_err(|e| store_err("read items", e))?
            .into_iter()
            .map(|m| ItemRef {
                item_type: m.item_type,
                item_id: m.item_id,
                created_by: m.created_by,
                before: m.before_json,
                after: m.after_json,
            })
            .collect())
    }
    async fn decisions(&self, runner: &DbTx<'a>, id: Uuid) -> Result<Vec<Decision>, ApprovalError> {
        decision_rows(runner, &self.scope, self.tenant_id, id)
            .await
            .map_err(|e| store_err("decisions", e))?
            .into_iter()
            .map(decision_from_model)
            .collect()
    }
    async fn insert_decision(&self, runner: &DbTx<'a>, d: &Decision) -> Result<(), ApprovalError> {
        let m = approval_decision::ActiveModel {
            unit_id: Set(d.unit_id),
            tenant_id: Set(self.tenant_id),
            actor: Set(d.actor),
            generation: Set(d.generation),
            decision: Set(d.verdict.as_str().into()),
            note: Set(d.note.clone()),
            at: Set(d.at),
            stale: Set(d.stale),
        };
        approval_decision::Entity::insert(m.clone())
            .secure()
            .scope_with_model(&self.scope, &m)
            .map_err(|e| store_err("decision scope", e))?
            .exec(runner)
            .await
            .map_err(|e| {
                // Two votes of one actor that both passed `already_voted`: the loser's 409
                // `DUPLICATE_VOTE`, never a store failure's 500 (RS-32).
                if e.is_unique_violation() {
                    ApprovalError::DuplicateVote
                } else {
                    store_err("insert decision", e)
                }
            })?;
        Ok(())
    }
    async fn refresh(
        &self,
        runner: &DbTx<'a>,
        id: Uuid,
        items: &[ItemRef],
        snapshot: &serde_json::Value,
        snapshot_hash: &str,
        generation: i32,
    ) -> Result<(), ApprovalError> {
        approval_unit_item::Entity::delete_many()
            .secure()
            .scope_with(&self.scope)
            .filter(item_key(self.tenant_id, id))
            .exec(runner)
            .await
            .map_err(|e| store_err("delete old items", e))?;
        self.insert_items(runner, id, items).await?;
        approval_unit::Entity::update_many()
            .secure()
            .scope_with(&self.scope)
            .col_expr(
                approval_unit::Column::Snapshot,
                Expr::value(snapshot.clone()),
            )
            .col_expr(
                approval_unit::Column::SnapshotHash,
                Expr::value(snapshot_hash),
            )
            .col_expr(approval_unit::Column::Generation, Expr::value(generation))
            .filter(unit_key(self.tenant_id, id))
            .exec(runner)
            .await
            .map_err(|e| store_err("refresh unit", e))?;
        approval_decision::Entity::update_many()
            .secure()
            .scope_with(&self.scope)
            .col_expr(approval_decision::Column::Stale, Expr::value(true))
            .filter(
                decision_key(self.tenant_id, id)
                    .add(approval_decision::Column::Generation.lt(generation)),
            )
            .exec(runner)
            .await
            .map_err(|e| store_err("stale votes", e))?;
        Ok(())
    }
    async fn set_state(
        &self,
        runner: &DbTx<'a>,
        id: Uuid,
        state: UnitState,
        decided_at: Option<OffsetDateTime>,
        note: Option<&str>,
    ) -> Result<(), ApprovalError> {
        approval_unit::Entity::update_many()
            .secure()
            .scope_with(&self.scope)
            .col_expr(approval_unit::Column::State, Expr::value(state.as_str()))
            .col_expr(approval_unit::Column::DecidedAt, Expr::value(decided_at))
            .col_expr(approval_unit::Column::DecidedNote, Expr::value(note))
            .filter(unit_key(self.tenant_id, id))
            .exec(runner)
            .await
            .map_err(|e| store_err("decide unit", e))?;
        Ok(())
    }
}
/// Read the explicit policy, falling back to quorum one when no default row exists.
/// # Errors
/// Returns scoped storage failures or a corrupt stored quorum.
pub async fn read_policy(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
) -> Result<Policy, RepoError> {
    let mut p = Policy {
        default_quorum: DEFAULT_QUORUM,
        overrides: std::collections::BTreeMap::new(),
    };
    for row in approval_policy::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(approval_policy::Column::TenantId.eq(tenant_id)))
        .all(runner)
        .await
        .map_err(|e| driver_failure("read policy".into(), e))?
    {
        let quorum = u32::try_from(row.quorum).map_err(|e| RepoError::CorruptRow(e.to_string()))?;
        if row.kind == "*" {
            p.default_quorum = quorum;
        } else {
            p.overrides.insert(row.kind, quorum);
        }
    }
    Ok(p)
}
/// Upsert a tenant's default or kind-specific quorum.
/// # Errors
/// Returns overflow or scoped storage failures.
pub async fn write_policy(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    kind: &str,
    quorum: u32,
) -> Result<(), RepoError> {
    let m = approval_policy::ActiveModel {
        tenant_id: Set(tenant_id),
        kind: Set(kind.into()),
        quorum: Set(i32::try_from(quorum).map_err(|e| RepoError::Db(e.to_string()))?),
    };
    let conflict = SecureOnConflict::<approval_policy::Entity>::columns([
        approval_policy::Column::TenantId,
        approval_policy::Column::Kind,
    ])
    .update_columns([approval_policy::Column::Quorum])
    .map_err(|e| driver_failure("policy conflict".into(), e))?;
    approval_policy::Entity::insert(m.clone())
        .secure()
        .scope_with_model(scope, &m)
        .map_err(|e| driver_failure("policy scope".into(), e))?
        .on_conflict(conflict)
        .exec(runner)
        .await
        .map_err(|e| driver_failure("write policy".into(), e))?;
    Ok(())
}
/// Remove one kind's override so the kind follows the default again (P-D-216); the default row
/// (`*`) is the door's refusal, never removed here.
/// # Errors
/// Returns scoped storage failures; the number of rows removed (0 when the kind has none).
pub async fn delete_policy(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    kind: &str,
) -> Result<u64, RepoError> {
    Ok(approval_policy::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(approval_policy::Column::TenantId.eq(tenant_id))
                .add(approval_policy::Column::Kind.eq(kind))
                .add(approval_policy::Column::Kind.ne("*")),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete policy".into(), e))?
        .rows_affected)
}
/// List the scoped approval queue, optionally restricted by state and kind.
/// # Errors
/// Returns scoped storage failures or corrupt unit data.
pub async fn list_units(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    state: Option<UnitState>,
    kind: Option<&str>,
    ref_id: Option<Uuid>,
) -> Result<Vec<Unit>, RepoError> {
    let mut c = Condition::all().add(approval_unit::Column::TenantId.eq(tenant_id));
    if let Some(s) = state {
        c = c.add(approval_unit::Column::State.eq(s.as_str()));
    }
    if let Some(k) = kind {
        c = c.add(approval_unit::Column::Kind.eq(k));
    }
    if let Some(id) = ref_id {
        c = c.add(approval_unit::Column::RefId.eq(id));
    }
    approval_unit::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(c)
        .order_by(approval_unit::Column::SubmittedAt, Order::Asc)
        .order_by(approval_unit::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list units".into(), e))?
        .into_iter()
        .map(|m| unit_from_model(m).map_err(|e| RepoError::CorruptRow(e.to_string())))
        .collect()
}
/// The fields of the unit list's pager (P-D-224): its one order, `submitted_at`, and the tie-break
/// `id`. The list takes no `$filter`; its narrowing is [`UnitListFilter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitListField {
    SubmittedAt,
    Id,
}
impl FilterField for UnitListField {
    const FIELDS: &'static [Self] = &[Self::SubmittedAt, Self::Id];
    fn name(&self) -> &'static str {
        match self {
            Self::SubmittedAt => "submitted_at",
            Self::Id => "id",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::SubmittedAt => FieldKind::DateTimeUtc,
            Self::Id => FieldKind::Uuid,
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
/// How the pager reads the unit row for each field.
pub struct UnitListMapping;
impl FieldToColumn<UnitListField> for UnitListMapping {
    type Column = approval_unit::Column;
    fn map_field(field: UnitListField) -> approval_unit::Column {
        match field {
            UnitListField::SubmittedAt => approval_unit::Column::SubmittedAt,
            UnitListField::Id => approval_unit::Column::Id,
        }
    }
}
impl ODataFieldMapping<UnitListField> for UnitListMapping {
    type Entity = approval_unit::Entity;
    fn extract_cursor_value(model: &approval_unit::Model, field: UnitListField) -> sea_orm::Value {
        match field {
            UnitListField::SubmittedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(model.submitted_at))
            }
            UnitListField::Id => sea_orm::Value::Uuid(Some(model.id)),
        }
    }
}
/// The unit list's page size: 200 by default, at most 500, as pricing's list (D-458, P-D-224).
pub const UNIT_PAGE: LimitCfg = LimitCfg {
    default: 200,
    max: 500,
};
/// What the unit list narrows the tenant's units by; the counts take the same (P-D-227).
#[derive(Debug, Clone, Default)]
pub struct UnitListFilter {
    pub state: Option<UnitState>,
    /// A kind products records: the door refuses any other before it builds the filter.
    pub kind: Option<ApprovalKind>,
    pub ref_id: Option<Uuid>,
}
impl UnitListFilter {
    /// The tenant's units this narrowing keeps: the one condition the list's page and the counts
    /// read by, so the two cannot count different sets (P-D-227).
    fn condition(&self, tenant_id: Uuid) -> Condition {
        let mut c = Condition::all().add(approval_unit::Column::TenantId.eq(tenant_id));
        if let Some(s) = self.state {
            c = c.add(approval_unit::Column::State.eq(s.as_str()));
        }
        if let Some(k) = self.kind {
            c = c.add(approval_unit::Column::Kind.eq(k.as_str()));
        }
        if let Some(id) = self.ref_id {
            c = c.add(approval_unit::Column::RefId.eq(id));
        }
        c
    }
}
/// A unit list read refused or failed.
#[derive(Debug)]
pub enum UnitListError {
    /// The query itself: a cursor the pager refuses (400).
    Query(toolkit_odata::Error),
    /// Storage; a driver failure keeps its message for the retry classifier.
    Repo(RepoError),
}
/// The unit list's order (P-D-227): `submitted_at`, then the unit id breaking a tie, both in
/// `direction`. The door sets it from `$orderby`; [`page_units`] reads it from the query alone.
pub fn submission_order(direction: SortDir) -> ODataOrderBy {
    ODataOrderBy(
        [UnitListField::SubmittedAt, UnitListField::Id]
            .into_iter()
            .map(|field| OrderKey {
                field: field.name().to_owned(),
                dir: direction,
            })
            .collect(),
    )
}
/// One page of the tenant's units under `scope`, narrowed by `filter`, in the query's one order
/// (the phase 9 review's R36): [`submission_order`] as the door set it, ascending when the query
/// names none (P-D-224), descending on request (P-D-227), the id breaking a tie in the direction
/// of the first key; a continuation follows the order its cursor carries. `limit` defaults to 200
/// and is clamped at 500. ONE statement.
/// # Errors
/// [`UnitListError::Query`] for a cursor the pager refuses; [`UnitListError::Repo`] for storage
/// and a stored row outside its closed sets.
pub async fn page_units(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    filter: &UnitListFilter,
    query: &ODataQuery,
) -> Result<Page<Unit>, UnitListError> {
    let mut query = query.clone();
    if query.cursor.is_none() && query.order.0.is_empty() {
        query.order = submission_order(SortDir::Asc);
    }
    let tie = query.order.0.first().map_or(SortDir::Asc, |key| key.dir);
    let select = approval_unit::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(filter.condition(tenant_id));
    paginate_odata_try::<
        UnitListField,
        UnitListMapping,
        approval_unit::Entity,
        Unit,
        _,
        RepoError,
        _,
    >(
        select,
        runner,
        &query,
        (UnitListField::Id.name(), tie),
        UNIT_PAGE,
        |m| unit_from_model(m).map_err(|e| RepoError::CorruptRow(e.to_string())),
    )
    .await
    .map_err(|e| match e {
        // The pager renders the driver's error as text; kept as a driver failure so the door's
        // retry still sees a serialization failure or a busy database by its message.
        PaginateOdataTryError::OData(toolkit_odata::Error::Db(message)) => {
            UnitListError::Repo(RepoError::Driver {
                context: "list units".into(),
                source: sea_orm::DbErr::Custom(message),
            })
        }
        PaginateOdataTryError::OData(other) => UnitListError::Query(other),
        PaginateOdataTryError::MapError(e) => UnitListError::Repo(e),
    })
}
/// One stored row of [`count_units`]: the units of one state and one kind.
#[derive(Debug, sea_orm::FromQueryResult)]
struct StateKindCount {
    state: String,
    kind: String,
    n: i64,
}
/// The units of one state and one kind, as [`count_units`] answers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitCount {
    pub state: UnitState,
    pub kind: ApprovalKind,
    pub units: u64,
}
/// The tenant's units under `scope` that `filter` keeps, counted by state and kind in ONE grouped
/// statement whatever their number (P-D-227): one row per pair that has a unit, its state and its
/// kind read through their closed sets, as every unit read reads them.
/// # Errors
/// Returns typed database failures; a stored state or kind outside its closed set, or a negative
/// count, is a corrupt row.
pub async fn count_units(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    filter: &UnitListFilter,
) -> Result<Vec<UnitCount>, RepoError> {
    use sea_orm::QuerySelect;
    approval_unit::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(filter.condition(tenant_id))
        .project_all(runner, |q| {
            q.select_only()
                .column(approval_unit::Column::State)
                .column(approval_unit::Column::Kind)
                .column_as(Expr::col(approval_unit::Column::Id).count(), "n")
                .group_by(approval_unit::Column::State)
                .group_by(approval_unit::Column::Kind)
                .into_model::<StateKindCount>()
        })
        .await
        .map_err(|e| driver_failure("count units".into(), e))?
        .into_iter()
        .map(|row| {
            let state = UnitState::parse(&row.state).ok_or_else(|| {
                RepoError::CorruptRow(format!("approval unit state {}", row.state))
            })?;
            let kind = ApprovalKind::parse(&row.kind).ok_or_else(|| {
                RepoError::CorruptRow(format!("approval units of unknown kind {}", row.kind))
            })?;
            let units = u64::try_from(row.n)
                .map_err(|_| RepoError::CorruptRow(format!("approval unit count {}", row.n)))?;
            Ok(UnitCount { state, kind, units })
        })
        .collect()
}
/// One row of [`item_authors_of_units`]: an item's unit and its author.
#[derive(Debug, sea_orm::FromQueryResult)]
struct ItemAuthor {
    unit_id: Uuid,
    created_by: Uuid,
}
/// The authors of the items of every unit among `units`, each unit's in [`Store::items`]' order,
/// in ONE statement whatever their number (P-D-228, the twin of pricing's), reading only each
/// item's unit and author: what whether a reader may approve a unit judges, never the items'
/// content (the phase 9 review's R73, R74). An empty list reads nothing, as
/// [`decisions_of_units`].
/// # Errors
/// Returns typed database failures.
pub async fn item_authors_of_units(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    units: &[Uuid],
) -> Result<BTreeMap<Uuid, Vec<Uuid>>, RepoError> {
    use sea_orm::QuerySelect;
    let mut grouped: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();
    if units.is_empty() {
        return Ok(grouped);
    }
    for row in approval_unit_item::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(approval_unit_item::Column::TenantId.eq(tenant_id))
                .add(approval_unit_item::Column::UnitId.is_in(units.iter().copied())),
        )
        .order_by(approval_unit_item::Column::UnitId, Order::Asc)
        .order_by(approval_unit_item::Column::ItemType, Order::Asc)
        .order_by(approval_unit_item::Column::ItemId, Order::Asc)
        .project_all(runner, |q| {
            q.select_only()
                .column(approval_unit_item::Column::UnitId)
                .column(approval_unit_item::Column::CreatedBy)
                .into_model::<ItemAuthor>()
        })
        .await
        .map_err(|e| driver_failure("read the item authors of units".into(), e))?
    {
        grouped.entry(row.unit_id).or_default().push(row.created_by);
    }
    Ok(grouped)
}
/// The decisions of every unit among `units`, each unit's by generation, instant and actor as
/// [`Store::decisions`] reads them, in ONE statement whatever their number (P-D-224).
/// # Errors
/// Returns typed database failures; a stored decision outside its set is a corrupt row.
pub async fn decisions_of_units(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    units: &[Uuid],
) -> Result<BTreeMap<Uuid, Vec<Decision>>, RepoError> {
    let mut grouped: BTreeMap<Uuid, Vec<Decision>> = BTreeMap::new();
    if units.is_empty() {
        return Ok(grouped);
    }
    for m in approval_decision::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(approval_decision::Column::TenantId.eq(tenant_id))
                .add(approval_decision::Column::UnitId.is_in(units.iter().copied())),
        )
        .order_by(approval_decision::Column::UnitId, Order::Asc)
        .order_by(approval_decision::Column::Generation, Order::Asc)
        .order_by(approval_decision::Column::At, Order::Asc)
        .order_by(approval_decision::Column::Actor, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("read the decisions of units".into(), e))?
    {
        let unit = m.unit_id;
        let decision = decision_from_model(m).map_err(|e| RepoError::CorruptRow(e.to_string()))?;
        grouped.entry(unit).or_default().push(decision);
    }
    Ok(grouped)
}
async fn decision_rows(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<Vec<approval_decision::Model>, ScopeError> {
    approval_decision::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(decision_key(tenant, id))
        .order_by(approval_decision::Column::Generation, Order::Asc)
        .order_by(approval_decision::Column::At, Order::Asc)
        .order_by(approval_decision::Column::Actor, Order::Asc)
        .all(runner)
        .await
}
/// Read decisions of all generations, including stale votes.
/// # Errors
/// Returns scoped storage failures or corrupt verdicts.
pub async fn decisions_of(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    unit_id: Uuid,
) -> Result<Vec<Decision>, RepoError> {
    decision_rows(runner, scope, tenant_id, unit_id)
        .await
        .map_err(|e| driver_failure("read decisions".into(), e))?
        .into_iter()
        .map(|m| decision_from_model(m).map_err(|e| RepoError::CorruptRow(e.to_string())))
        .collect()
}
#[cfg(test)]
#[path = "approval_repo_tests.rs"]
mod approval_repo_tests;

/// Read an authorized unit without taking a write transaction.
/// # Errors
/// Returns typed storage or corrupt-row errors.
pub async fn find_unit(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<Option<Unit>, ApprovalError> {
    approval_unit::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(unit_key(tenant, id))
        .one(runner)
        .await
        .map_err(|e| store_err("read unit", e))?
        .map(unit_from_model)
        .transpose()
}
