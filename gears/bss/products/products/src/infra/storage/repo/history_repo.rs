//! A SKU's history on the toolkit's `OData` pager (P-D-213): the audit rows whose subject is the
//! SKU, and those whose subject is one of the SKU's approval units (`ref_id` the SKU), ordered by
//! `audit_id` alone. Every writer mints it as a UUID v7 inside the act's transaction (per attempt),
//! so its order is the order the acts wrote — bytes on `SQLite`, `uuid` on Postgres, both compared
//! in time order. `written_at` is not the order: it is the instant the act began, taken before its
//! transaction and kept across a retry, and on `SQLite` its RFC 3339 text does not sort as time
//! within one second.
use super::{SkuListError, driver_failure};
use crate::infra::storage::{
    RepoError,
    entity::{approval_unit, audit_log},
};
use bss_products_sdk::models::Lifecycle;
use sea_orm::sea_query::Query;
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait};
use std::collections::{HashMap, HashSet};
use time::OffsetDateTime;
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, LimitCfg, ODataFieldMapping, PaginateOdataTryError, paginate_odata_try,
};
use toolkit_db::secure::{AccessScope, DBRunner, SecureEntityExt};
use toolkit_odata::filter::{FieldKind, FilterField, FilterOp, ODataValue};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use uuid::Uuid;

/// The page size when the caller names none, and the most a page holds (`$top` is clamped).
pub const HISTORY_PAGE: LimitCfg = LimitCfg {
    default: 50,
    max: 200,
};

/// The one key the history orders by; it takes no `$filter` and no `$orderby`. A cursor that
/// names any other key (one minted when the history ordered by `written_at` first) is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryField {
    AuditId,
}
impl FilterField for HistoryField {
    const FIELDS: &'static [Self] = &[Self::AuditId];
    fn name(&self) -> &'static str {
        match self {
            Self::AuditId => "audit_id",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::AuditId => FieldKind::Uuid,
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
/// How the pager reads the audit row for each key.
pub struct HistoryMapping;
impl FieldToColumn<HistoryField> for HistoryMapping {
    type Column = audit_log::Column;
    fn map_field(field: HistoryField) -> audit_log::Column {
        match field {
            HistoryField::AuditId => audit_log::Column::AuditId,
        }
    }
    /// The history filters by its SKU alone; the door refuses `$filter` before the pager sees it.
    fn map_value(
        field: HistoryField,
        _op: FilterOp,
        _value: &ODataValue,
    ) -> Result<ODataValue, String> {
        Err(format!(
            "`{}` orders the history and is not a filter field",
            field.name()
        ))
    }
}
impl ODataFieldMapping<HistoryField> for HistoryMapping {
    type Entity = audit_log::Entity;
    fn extract_cursor_value(model: &audit_log::Model, field: HistoryField) -> sea_orm::Value {
        match field {
            HistoryField::AuditId => sea_orm::Value::Uuid(Some(model.audit_id)),
        }
    }
}

/// One act in a SKU's history: when, who, what, the lifecycle it found and left (null on a row
/// written before `m20260927_000008`), the unit it concerned, and the note it carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkuHistoryEntry {
    pub at: OffsetDateTime,
    pub actor: Uuid,
    pub action: String,
    pub from_lifecycle: Option<Lifecycle>,
    pub to_lifecycle: Option<Lifecycle>,
    pub unit_id: Option<Uuid>,
    pub unit_kind: Option<String>,
    pub note: Option<String>,
}

fn lifecycle(value: Option<&str>) -> Result<Option<Lifecycle>, RepoError> {
    value
        .map(|v| {
            Lifecycle::parse(v)
                .ok_or_else(|| RepoError::CorruptRow(format!("audit row lifecycle {v}")))
        })
        .transpose()
}

/// One history row before its legacy `retiring` tokens are mapped (P-D-248).
struct Staged {
    audit_id: Uuid,
    at: OffsetDateTime,
    actor: Uuid,
    action: String,
    from_raw: Option<String>,
    to_raw: Option<String>,
    unit_id: Option<Uuid>,
    note: Option<String>,
}
/// A row the mapping may consult: the page, and every audit row of the page's units.
struct MoveRow {
    audit_id: Uuid,
    action: String,
    from: Option<String>,
    to: Option<String>,
    unit_id: Option<Uuid>,
}

fn remember(seen: &mut HashSet<Uuid>, context: &mut Vec<MoveRow>, row: MoveRow) {
    if seen.insert(row.audit_id) {
        context.push(row);
    }
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "the pager maps a row through a Result, and this step does not fail"
)]
fn stage(m: audit_log::Model) -> Result<Staged, RepoError> {
    let unit_id = (m.subject_kind == "approval_unit")
        .then_some(m.subject_id)
        .flatten();
    Ok(Staged {
        audit_id: m.audit_id,
        at: m.written_at,
        actor: m.actor_ref,
        action: m.action,
        from_raw: m.from_lifecycle,
        to_raw: m.to_lifecycle,
        unit_id,
        note: m.reason,
    })
}
fn both(token: Option<&str>) -> (Option<String>, Option<String>) {
    let owned = token.map(str::to_owned);
    (owned.clone(), owned)
}
const LEGACY_RETIRING: &str = "retiring";
const RETIRED: &str = "retired";
const ACTION_SUBMIT: &str = "approval.submit";
const ACTION_VOTE: &str = "approval.vote";
const ACTION_REFRESHED: &str = "approval.refreshed";
const ACTION_APPLIED: &str = "approval.applied";
const ACTION_APPROVED: &str = "approval.approved";
const ACTION_REJECTED: &str = "approval.rejected";
const ACTION_WITHDRAWN: &str = "approval.withdrawn";
const ACTION_UNFENCE: &str = "sku.unfence";
const ACTION_FENCE_EXPIRED: &str = "sku.fence_expired";

fn is_retiring(token: Option<&str>) -> bool {
    token == Some(LEGACY_RETIRING)
}

fn non_retiring<'a>(from: Option<&'a str>, to: Option<&'a str>) -> Option<&'a str> {
    [from, to]
        .into_iter()
        .flatten()
        .find(|token| *token != LEGACY_RETIRING)
}
/// The lifecycle a retire submit really left, when its stored `from` is already `retiring`:
/// the earlier row that entered `retiring`, or nothing when that row is not in hand.
fn entered_from(context: &[MoveRow], before: Uuid) -> Option<String> {
    context
        .iter()
        .filter(|row| {
            row.audit_id < before
                && is_retiring(row.to.as_deref())
                && !is_retiring(row.from.as_deref())
        })
        .max_by_key(|row| row.audit_id)
        .and_then(|row| row.from.clone())
}
fn submit_lifecycle(context: &[MoveRow], unit: Uuid) -> Option<String> {
    let submit = context
        .iter()
        .filter(|row| row.unit_id == Some(unit) && row.action == ACTION_SUBMIT)
        .min_by_key(|row| row.audit_id)?;
    match submit.from.as_deref() {
        Some(LEGACY_RETIRING) => entered_from(context, submit.audit_id),
        other if is_retiring(submit.to.as_deref()) => other.map(str::to_owned),
        other => non_retiring(other, submit.to.as_deref()).map(str::to_owned),
    }
}
/// Map a legacy `retiring` token on the raw strings, before [`Lifecycle::parse`] (P-D-248). A
/// token that remains is a corrupt row; the mapping leaves none.
fn map_legacy(row: &Staged, context: &[MoveRow]) -> (Option<String>, Option<String>) {
    let from = row.from_raw.as_deref();
    let to = row.to_raw.as_deref();
    if !is_retiring(from) && !is_retiring(to) {
        return (row.from_raw.clone(), row.to_raw.clone());
    }
    let resolved = row.unit_id.and_then(|id| submit_lifecycle(context, id));
    match row.action.as_str() {
        ACTION_SUBMIT if is_retiring(to) && !is_retiring(from) => both(from),
        ACTION_SUBMIT => match resolved.or_else(|| entered_from(context, row.audit_id)) {
            Some(lifecycle) => both(Some(lifecycle.as_str())),
            None => (None, None),
        },
        ACTION_VOTE | ACTION_REFRESHED => match resolved {
            Some(lifecycle) => both(Some(lifecycle.as_str())),
            None => (None, None),
        },
        ACTION_APPLIED | ACTION_APPROVED if is_retiring(from) && to == Some(RETIRED) => {
            match resolved {
                Some(lifecycle) => (Some(lifecycle), Some(RETIRED.to_owned())),
                None => (None, Some(RETIRED.to_owned())),
            }
        }
        ACTION_REJECTED | ACTION_WITHDRAWN | ACTION_UNFENCE | ACTION_FENCE_EXPIRED => {
            match non_retiring(from, to).map(str::to_owned).or(resolved) {
                Some(lifecycle) => both(Some(lifecycle.as_str())),
                None => (None, None),
            }
        }
        _ if is_retiring(to) && !is_retiring(from) => both(from),
        _ if is_retiring(from) && to == Some(RETIRED) => match resolved {
            Some(lifecycle) => (Some(lifecycle), Some(RETIRED.to_owned())),
            None => (None, Some(RETIRED.to_owned())),
        },
        _ => match non_retiring(from, to).map(str::to_owned).or(resolved) {
            Some(lifecycle) => both(Some(lifecycle.as_str())),
            None => (None, None),
        },
    }
}
fn entry_of(row: &Staged, context: &[MoveRow]) -> Result<SkuHistoryEntry, RepoError> {
    let (from, to) = map_legacy(row, context);
    Ok(SkuHistoryEntry {
        at: row.at,
        actor: row.actor,
        action: row.action.clone(),
        from_lifecycle: lifecycle(from.as_deref())?,
        to_lifecycle: lifecycle(to.as_deref())?,
        unit_id: row.unit_id,
        unit_kind: None,
        note: row.note.clone(),
    })
}

/// The SKU's rows: its own, and its units' (`ref_id` the SKU), in the tenant.
fn history_condition(tenant: Uuid, sku: Uuid) -> Condition {
    let units = Query::select()
        .column(approval_unit::Column::Id)
        .from(approval_unit::Entity)
        .and_where(approval_unit::Column::TenantId.eq(tenant))
        .and_where(approval_unit::Column::RefId.eq(sku))
        .to_owned();
    Condition::all()
        .add(audit_log::Column::TenantId.eq(tenant))
        .add(
            Condition::any()
                .add(
                    Condition::all()
                        .add(audit_log::Column::SubjectKind.eq("sku"))
                        .add(audit_log::Column::SubjectId.eq(sku)),
                )
                .add(
                    Condition::all()
                        .add(audit_log::Column::SubjectKind.eq("approval_unit"))
                        .add(audit_log::Column::SubjectId.in_subquery(units)),
                ),
        )
}
/// Every audit row of the SKU whose stored lifecycle is the legacy `retiring` token: the SKU's
/// own rows and its units' rows. One statement, so an enter on an earlier unit is in hand when
/// the page that holds a later retire does not (P-D-248).
fn retiring_moves(tenant: Uuid, sku: Uuid) -> Condition {
    history_condition(tenant, sku).add(
        Condition::any()
            .add(audit_log::Column::FromLifecycle.eq(LEGACY_RETIRING))
            .add(audit_log::Column::ToLifecycle.eq(LEGACY_RETIRING)),
    )
}

/// One page of the SKU's history in `audit_id` order — the order the acts wrote (the query names
/// no order; a cursor carries its own), `$top` 50 by default and clamped at 200, each unit row
/// with its unit's kind from ONE read of the page's units. The caller has found the SKU in its
/// scope.
/// # Errors
/// [`SkuListError::Query`] for a cursor the pager refuses; [`SkuListError::Repo`] for storage.
/// A stored `retiring` is mapped before it is parsed (P-D-248), so it is never a 500. A page
/// that still holds one loads every `retiring` row of the SKU first, not only the page's units.
pub async fn page_sku_history(
    runner: &impl DBRunner,
    tenant: Uuid,
    sku: Uuid,
    query: &ODataQuery,
) -> Result<Page<SkuHistoryEntry>, SkuListError> {
    let mut query = query.clone();
    if query.cursor.is_none() {
        query.order = ODataOrderBy(vec![OrderKey {
            field: HistoryField::AuditId.name().to_owned(),
            dir: SortDir::Asc,
        }]);
    }
    let scope = AccessScope::for_tenant(tenant);
    let select = audit_log::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(history_condition(tenant, sku));
    let page = paginate_odata_try::<
        HistoryField,
        HistoryMapping,
        audit_log::Entity,
        Staged,
        _,
        RepoError,
        _,
    >(
        select,
        runner,
        &query,
        (HistoryField::AuditId.name(), SortDir::Asc),
        HISTORY_PAGE,
        stage,
    )
    .await
    .map_err(|e| match e {
        // Kept as a driver failure so the retry classifier still reads the driver's message.
        PaginateOdataTryError::OData(toolkit_odata::Error::Db(message)) => {
            SkuListError::Repo(RepoError::Driver {
                context: "read SKU history".into(),
                source: DbErr::Custom(message),
            })
        }
        PaginateOdataTryError::OData(other) => SkuListError::Query(other),
        PaginateOdataTryError::MapError(e) => SkuListError::Repo(e),
    })?;
    let mut units: Vec<Uuid> = page.items.iter().filter_map(|e| e.unit_id).collect();
    units.sort_unstable();
    units.dedup();
    let mut context: Vec<MoveRow> = page
        .items
        .iter()
        .map(|row| MoveRow {
            audit_id: row.audit_id,
            action: row.action.clone(),
            from: row.from_raw.clone(),
            to: row.to_raw.clone(),
            unit_id: row.unit_id,
        })
        .collect();
    let mut seen: HashSet<Uuid> = context.iter().map(|row| row.audit_id).collect();
    let legacy = page
        .items
        .iter()
        .any(|row| is_retiring(row.from_raw.as_deref()) || is_retiring(row.to_raw.as_deref()));
    if legacy && !units.is_empty() {
        let unit_rows = audit_log::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(audit_log::Column::TenantId.eq(tenant))
                    .add(audit_log::Column::SubjectKind.eq("approval_unit"))
                    .add(audit_log::Column::SubjectId.is_in(units.clone())),
            )
            .all(runner)
            .await
            .map_err(|e| SkuListError::Repo(driver_failure("read history unit rows".into(), e)))?;
        for row in unit_rows {
            remember(
                &mut seen,
                &mut context,
                MoveRow {
                    audit_id: row.audit_id,
                    action: row.action,
                    from: row.from_lifecycle,
                    to: row.to_lifecycle,
                    unit_id: (row.subject_kind == "approval_unit")
                        .then_some(row.subject_id)
                        .flatten(),
                },
            );
        }
    }
    let kinds: HashMap<Uuid, String> = if units.is_empty() {
        HashMap::new()
    } else {
        approval_unit::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(approval_unit::Column::TenantId.eq(tenant))
                    .add(approval_unit::Column::Id.is_in(units)),
            )
            .all(runner)
            .await
            .map_err(|e| SkuListError::Repo(driver_failure("read history units".into(), e)))?
            .into_iter()
            // A unit's kind is read through its closed set here too: a kind products does not
            // record is a corrupt row, as on every unit read (the phase 9 review's theme C).
            .map(|u| {
                crate::domain::approvals::ApprovalKind::parse(&u.kind)
                    .map(|k| (u.id, k.as_str().to_owned()))
                    .ok_or_else(|| {
                        SkuListError::Repo(RepoError::CorruptRow(format!(
                            "approval unit {} has unknown kind {}",
                            u.id, u.kind
                        )))
                    })
            })
            .collect::<Result<_, _>>()?
    };
    if legacy {
        let retiring = audit_log::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(retiring_moves(tenant, sku))
            .all(runner)
            .await
            .map_err(|e| SkuListError::Repo(driver_failure("read retiring history".into(), e)))?;
        for row in retiring {
            remember(
                &mut seen,
                &mut context,
                MoveRow {
                    audit_id: row.audit_id,
                    action: row.action,
                    from: row.from_lifecycle,
                    to: row.to_lifecycle,
                    unit_id: (row.subject_kind == "approval_unit")
                        .then_some(row.subject_id)
                        .flatten(),
                },
            );
        }
    }
    let items = page
        .items
        .iter()
        .map(|row| {
            let mut entry = entry_of(row, &context).map_err(SkuListError::Repo)?;
            entry.unit_kind = row.unit_id.and_then(|id| kinds.get(&id).cloned());
            Ok(entry)
        })
        .collect::<Result<Vec<_>, SkuListError>>()?;
    Ok(Page {
        items,
        page_info: page.page_info,
    })
}

#[cfg(test)]
#[path = "history_repo_tests.rs"]
mod history_repo_tests;
