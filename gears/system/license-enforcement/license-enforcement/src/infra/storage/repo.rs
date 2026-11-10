//! Queries of the gear store. Every query goes through the Secure ORM.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use async_trait::async_trait;
use gts::GtsTypeId;
use license_enforcement_sdk::dto::{GrantFilterField, PackFilterField};
use license_enforcement_sdk::error::{precondition, reason};
use license_enforcement_sdk::models::{
    Binding, BindingCounts, Conditions, Grant, GrantState, Pack, Permissions, ResourceRef,
    SubjectRef, TerminationReason, Validity, Window,
};
use license_enforcement_sdk::{
    BindingFilter, DelegationId, EvidenceId, Expect, GrantId, Intake, MaterialWrite, PackId,
    RejectionReason, StoredEvidence,
};
use sea_orm::sea_query::{Alias, CaseStatement, Expr, ExprTrait, LikeExpr, Query, SimpleExpr};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, QueryFilter, QuerySelect, Set};
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::odata::{LimitCfg, filter_node_to_condition, paginate_odata};
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureOnConflict, SecureUpdateExt,
    TxAccessMode, TxConfig, TxIsolationLevel, secure_insert, secure_insert_many,
};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::entity::{evidence, grant, idempotency, pack, plugin_material};
use super::migrations::{UQ_EVIDENCE_EXTERNAL_ID, UQ_EVIDENCE_REPLACES, UQ_EVIDENCE_REPLAY_KEY};
use super::odata::{GrantODataMapper, PackMemberMapper, PackODataMapper};
use crate::domain::error::DomainError;
use crate::domain::grants::{PackCount, subject_loses_grant};
use crate::domain::repo::{Caller, Candidate, GrantRepo, NewEvidenceRow};
use crate::domain::types::{PackOrderField, type_key};

const TERMINATED: &str = "terminated";

fn db(e: ScopeError) -> DomainError {
    match e {
        ScopeError::TenantNotInScope { .. } | ScopeError::Denied(_) => {
            DomainError::PermissionDenied
        }
        other => DomainError::store(other),
    }
}

/// The reuse that a unique violation on evidence reports, by the index it
/// names: PostgreSQL gives the index name, SQLite the columns. `None` for
/// another error.
///
/// The index is read from the message of a violation that the driver already
/// classified by its code (as `chat-engine` does); an index that the text does
/// not name is `AdmissionConflict`.
fn evidence_reuse(e: &sea_orm::DbErr) -> Option<DomainError> {
    let Some(sea_orm::SqlErr::UniqueConstraintViolation(message)) = e.sql_err() else {
        return None;
    };
    let names = |index: &str, column: &str| {
        message.contains(index)
            || message.contains(&format!("license_enforcement__evidence.{column}"))
    };
    Some(if names(UQ_EVIDENCE_REPLAY_KEY, "replay_key") {
        DomainError::invalid(
            "idempotency_token",
            "the token already issued grants and its idempotency window is over; use a new token",
            reason::TOKEN_REUSED,
        )
    } else if names(UQ_EVIDENCE_EXTERNAL_ID, "external_id") {
        DomainError::EvidenceRejected(RejectionReason::Replay)
    } else if names(UQ_EVIDENCE_REPLACES, "replaces_evidence_id") {
        DomainError::EvidenceChainBroken(precondition::EVIDENCE_ALREADY_REPLACED)
    } else {
        tracing::warn!(
            error = %e,
            "license-enforcement: a unique violation on evidence names no known index"
        );
        DomainError::AdmissionConflict
    })
}

/// The wire string of a unit enum, as serde writes it.
fn wire<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

fn from_wire<T: DeserializeOwned>(column: &str, value: &str) -> Result<T, DomainError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|e| DomainError::Internal(format!("stored {column} '{value}': {e}")))
}

/// A stored integer in the range of `T`.
fn stored_int<T: TryFrom<i64>>(column: &str, value: i64) -> Result<T, DomainError> {
    T::try_from(value)
        .map_err(|_| DomainError::Internal(format!("stored {column} '{value}' is out of range")))
}

fn counts_json(counts: &[BindingCounts]) -> Result<serde_json::Value, DomainError> {
    serde_json::to_value(counts).map_err(|e| DomainError::Internal(format!("pack counts: {e}")))
}

/// The live chain of each `(grant, its root)` pair, from one read of the live
/// rows of those roots. Terminated rows are not read: the live rows of a tree
/// are one path of at most `MAX_DELEGATION_DEPTH` + 1 rows (I3), while
/// terminated ones pile up with every recall and return until the sweeper.
async fn walk_trees<C: DBRunner>(
    runner: &C,
    starts: &[(GrantId, GrantId)],
) -> Result<HashMap<GrantId, Vec<Grant>>, DomainError> {
    let roots: BTreeSet<Uuid> = starts.iter().map(|(_, r)| r.0).collect();
    if roots.is_empty() {
        return Ok(HashMap::new());
    }
    let filter = Condition::all()
        .add(grant::Column::RootGrantId.is_in(roots))
        .add(state_is_not_terminated());
    // SAFETY(cross-tenant read): the trees of roots whose grants the caller
    // already reached; their links are held by other tenants.
    let tree: HashMap<Uuid, grant::Model> = grant::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(filter)
        .all(runner)
        .await
        .map_err(db)?
        .into_iter()
        .map(|r| (r.id, r))
        .collect();
    let mut out = HashMap::with_capacity(starts.len());
    for (start, _) in starts {
        let mut chain = Vec::new();
        // A cycle in edited rows ends the walk: each row is taken once.
        let mut seen = HashSet::new();
        let mut next = Some(start.0);
        while let Some(id) = next.filter(|id| seen.insert(*id)) {
            let Some(row) = tree.get(&id) else {
                break;
            };
            next = row.parent_grant_id;
            chain.push(to_grant(row.clone())?);
        }
        out.insert(*start, chain);
    }
    Ok(out)
}

/// At most `batch` rows that `eligible` admits, the oldest by `updated_at`,
/// locked in id order as every writer locks. The first read follows the
/// partial index on `updated_at` without a lock; the locked read checks
/// `eligible` again, since a row can change between the two.
async fn oldest_locked<C: DBRunner>(
    runner: &C,
    eligible: impl Fn() -> Condition,
    batch: u64,
) -> Result<Vec<IdAndPack>, DomainError> {
    // SAFETY(cross-tenant): retention is a store task for every tenant.
    let all = AccessScope::allow_all();
    let oldest: Vec<Uuid> = grant::Entity::find()
        .secure()
        .scope_with(&all)
        .filter(eligible())
        .order_by(grant::Column::UpdatedAt, Order::Asc)
        .order_by(grant::Column::Id, Order::Asc)
        .limit(batch)
        .project_all(runner, |q| {
            q.select_only()
                .column(grant::Column::Id)
                .into_model::<IdOnly>()
        })
        .await
        .map_err(db)?
        .into_iter()
        .map(|r| r.id)
        .collect();
    if oldest.is_empty() {
        return Ok(Vec::new());
    }
    grant::Entity::find()
        .secure()
        .scope_with(&all)
        .filter(eligible().add(grant::Column::Id.is_in(oldest)))
        .order_by(grant::Column::Id, Order::Asc)
        .lock_exclusive()
        .project_all(runner, |q| {
            q.select_only()
                .column(grant::Column::Id)
                .column(grant::Column::PackId)
                .into_model::<IdAndPack>()
        })
        .await
        .map_err(db)
}

/// Delete at most `batch` terminated sub-grants without children that last
/// changed before `cutoff`: the leaves of the returned and recalled branches.
/// A branch goes leaf first over the batches, so a parent
/// never leaves before its subtree (DESIGN §3.7).
async fn sweep_branches<C: DBRunner>(
    runner: &C,
    cutoff: OffsetDateTime,
    batch: u64,
) -> Result<u64, DomainError> {
    use toolkit_db::secure::SecureDeleteExt;
    // SAFETY(cross-tenant): retention is a store task for every tenant.
    let all = AccessScope::allow_all();
    let eligible = || {
        let child = Alias::new("child");
        let any_child = Query::select()
            .expr(Expr::val(1))
            .from_as(grant::Entity, child.clone())
            .and_where(
                Expr::col((child, grant::Column::ParentGrantId))
                    .equals((grant::Entity, grant::Column::Id)),
            )
            .to_owned();
        Condition::all()
            .add(grant::Column::ParentGrantId.is_not_null())
            .add(state_is_terminated())
            .add(grant::Column::UpdatedAt.lte(utc(cutoff)))
            .add(Expr::exists(any_child).not())
    };
    // A delegation verb writes the version of terminated rows: they are locked
    // in id order, as every writer does.
    let ids: Vec<Uuid> = oldest_locked(runner, eligible, batch)
        .await?
        .into_iter()
        .map(|r| r.id)
        .collect();
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(grant::Entity::delete_many()
        .filter(Condition::all().add(grant::Column::Id.is_in(ids)))
        .secure()
        .scope_with(&all)
        .exec(runner)
        .await
        .map_err(db)?
        .rows_affected)
}

/// A member that moved from one count of its pack to another: its resource,
/// and the counts before and after (`None` when terminated).
type CountMove = (ResourceRef, Option<PackCount>, Option<PackCount>);

/// Apply `moves` to the counts of pack `id`: one locked read and one write.
async fn move_counts<C: DBRunner>(
    runner: &C,
    id: PackId,
    holder: Uuid,
    moves: &[CountMove],
) -> Result<(), DomainError> {
    // SAFETY(own tenant): the counts are derived data of the members, held by
    // the holder of their pack; the write goes under the decision on the
    // members (DESIGN §3.2).
    let scope = AccessScope::for_tenant(holder);
    let row = pack::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(pack::Column::Id.eq(id.0)))
        .lock_exclusive()
        .one(runner)
        .await
        .map_err(db)?
        .ok_or_else(|| DomainError::Internal(format!("pack {id} of a member is missing")))?;
    let mut counts = stored_counts(row.id, row.counts)?;
    for (resource, from, to) in moves {
        let entry = counts
            .iter_mut()
            .find(|c| c.resource == *resource)
            .ok_or_else(|| DomainError::Internal(format!("pack {id} counts no such resource")))?;
        if let Some(k) = from {
            let n = k.of_counts(entry);
            *n = n.checked_sub(1).ok_or_else(|| {
                DomainError::Internal(format!("pack {id} counts went below zero"))
            })?;
        }
        if let Some(k) = to {
            *k.of_counts(entry) += 1;
        }
    }
    pack::Entity::update_many()
        .col_expr(pack::Column::Counts, Expr::value(counts_json(&counts)?))
        .filter(Condition::all().add(pack::Column::Id.eq(id.0)))
        .secure()
        .scope_with(&scope)
        .exec(runner)
        .await
        .map_err(db)?;
    Ok(())
}

/// Rows per `UPDATE`: each row binds two values per column, well below the
/// bind limit of `SQLite`.
const UPDATE_CHUNK: usize = 100;

/// The columns that a verb can change, as `to_active` encodes them.
fn verb_columns(
    g: &Grant,
    now: OffsetDateTime,
) -> Result<Vec<(grant::Column, sea_orm::Value)>, DomainError> {
    let am = to_active(g, g.id, now);
    [
        (grant::Column::BindingKind, am.binding_kind.into_value()),
        (
            grant::Column::BindingSubjectType,
            am.binding_subject_type.into_value(),
        ),
        (
            grant::Column::BindingSubjectTypeKey,
            am.binding_subject_type_key.into_value(),
        ),
        (
            grant::Column::BindingSubjectId,
            am.binding_subject_id.into_value(),
        ),
        (
            grant::Column::BindingGrantId,
            am.binding_grant_id.into_value(),
        ),
        (
            grant::Column::BindingTenantId,
            am.binding_tenant_id.into_value(),
        ),
        (grant::Column::ValidityKind, am.validity_kind.into_value()),
        (grant::Column::NotBefore, am.not_before.into_value()),
        (grant::Column::ExpiresAt, am.expires_at.into_value()),
        (grant::Column::GraceUntil, am.grace_until.into_value()),
        (grant::Column::EffectiveEnd, am.effective_end.into_value()),
        (grant::Column::State, am.state.into_value()),
        (
            grant::Column::ParentGrantId,
            am.parent_grant_id.into_value(),
        ),
        (grant::Column::EvidenceId, am.evidence_id.into_value()),
        (
            grant::Column::TerminationReason,
            am.termination_reason.into_value(),
        ),
        (grant::Column::Version, am.version.into_value()),
        (grant::Column::UpdatedAt, am.updated_at.into_value()),
    ]
    .into_iter()
    .map(|(column, value)| {
        value
            .map(|v| (column, v))
            .ok_or_else(|| DomainError::Internal(format!("{column:?} unset")))
    })
    .collect()
}

/// Write `rows`, each only if its stored version is the one before its own:
/// one `UPDATE` per chunk, with a `CASE` on the id for each column.
async fn update_rows<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    rows: &[Grant],
    now: OffsetDateTime,
) -> Result<(), DomainError> {
    // One statement matches a row once: a second write of it would read as
    // a version conflict.
    let mut ids = HashSet::with_capacity(rows.len());
    if let Some(g) = rows.iter().find(|g| !ids.insert(g.id)) {
        return Err(DomainError::Internal(format!(
            "grant {} is written twice",
            g.id
        )));
    }
    for chunk in rows.chunks(UPDATE_CHUNK) {
        let mut cases: Vec<(grant::Column, Option<CaseStatement>)> = Vec::new();
        // The subject that lost the grant stays for the reason a later check
        // gives.
        let mut ended: [(grant::Column, grant::Column, Option<CaseStatement>); 2] = [
            (
                grant::Column::EndedSubjectTypeKey,
                grant::Column::BindingSubjectTypeKey,
                None,
            ),
            (
                grant::Column::EndedSubjectId,
                grant::Column::BindingSubjectId,
                None,
            ),
        ];
        let mut matched = Condition::any();
        for g in chunk {
            if g.termination_reason.is_some_and(subject_loses_grant)
                && matches!(g.binding, Binding::None)
            {
                for (column, from, slot) in &mut ended {
                    let this = grant::Column::Id.eq(g.id.0);
                    // A row that is already terminated has no subject left.
                    let kept = Expr::col(*from).if_null(Expr::col(*column));
                    *slot = Some(match slot.take() {
                        None => Expr::case(this, kept),
                        Some(case) => case.case(this, kept),
                    });
                }
            }
            let previous = i64::try_from(g.version.saturating_sub(1)).unwrap_or(i64::MAX);
            for (i, (column, value)) in verb_columns(g, now)?.into_iter().enumerate() {
                if cases.len() == i {
                    cases.push((column, None));
                }
                let this = grant::Column::Id.eq(g.id.0);
                let slot = &mut cases[i].1;
                *slot = Some(match slot.take() {
                    None => Expr::case(this, Expr::value(value)),
                    Some(case) => case.case(this, Expr::value(value)),
                });
            }
            matched = matched.add(
                Condition::all()
                    .add(grant::Column::Id.eq(g.id.0))
                    .add(grant::Column::Version.eq(previous)),
            );
        }
        let mut update = grant::Entity::update_many();
        for (column, case) in cases {
            if let Some(case) = case {
                update = update.col_expr(column, case.finally(Expr::col(column)).into());
            }
        }
        for (column, _, case) in ended {
            if let Some(case) = case {
                update = update.col_expr(column, case.finally(Expr::col(column)).into());
            }
        }
        let result = update
            .filter(matched)
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db)?;
        if result.rows_affected != chunk.len() as u64 {
            return Err(DomainError::VersionMismatch);
        }
    }
    Ok(())
}

fn stored_counts(id: Uuid, counts: serde_json::Value) -> Result<Vec<BindingCounts>, DomainError> {
    serde_json::from_value(counts)
        .map_err(|e| DomainError::Internal(format!("stored counts of pack {id}: {e}")))
}

/// A pack as the API shows it: a resource without live members is left out.
fn to_pack(m: pack::Model) -> Result<Pack, DomainError> {
    let mut counts = stored_counts(m.id, m.counts)?;
    counts.retain(|c| c.unbound + c.bound + c.delegated > 0);
    Ok(Pack::new(
        PackId(m.id),
        m.holder_tenant_id,
        counts,
        stored_int("version", m.version)?,
    ))
}

/// A column that the binding kind of the row requires.
fn required<T>(value: Option<T>, what: &str) -> Result<T, DomainError> {
    value.ok_or_else(|| DomainError::Internal(format!("stored binding without its {what}")))
}

fn to_grant(m: grant::Model) -> Result<Grant, DomainError> {
    let binding = match m.binding_kind.as_str() {
        "none" => Binding::None,
        "subject" => Binding::subject(SubjectRef {
            gts_type: GtsTypeId::new(required(m.binding_subject_type.as_deref(), "subject type")?),
            id: required(m.binding_subject_id, "subject id")?,
        }),
        "delegated" => Binding::delegated(
            GrantId(required(m.binding_grant_id, "grant id")?),
            required(m.binding_tenant_id, "tenant id")?,
        ),
        other => {
            return Err(DomainError::Internal(format!(
                "stored binding kind '{other}'"
            )));
        }
    };
    let window = Window {
        not_before: m.not_before,
        expires_at: m.expires_at,
        grace_until: m.grace_until,
    };
    let validity = if m.validity_kind == "inherited" {
        Validity::Inherited { window }
    } else {
        Validity::Own { window }
    };
    let mut g = Grant::new(
        GrantId(m.id),
        m.holder_tenant_id,
        ResourceRef {
            gts_type: GtsTypeId::new(&m.resource_type),
            id: m.resource_id,
        },
        GtsTypeId::new(&m.subject_type),
        validity,
        Permissions {
            may_bind: m.may_bind,
            max_depth: stored_int("max_depth", m.max_depth.into())?,
            requires_acceptance: m.requires_acceptance,
        },
        EvidenceId(m.evidence_id),
    );
    g.binding = binding;
    g.binding_fixed = m.binding_fixed;
    g.conditions = m.conditions.map(Conditions);
    g.recipient_rules = m.recipient_rules.map(Conditions);
    g.parent_grant_id = m.parent_grant_id.map(GrantId);
    g.grantor_tenant_id = m.grantor_tenant_id;
    g.delegation_id = m.delegation_id.map(Into::into);
    g.pack_id = m.pack_id.map(PackId);
    g.state = from_wire("state", &m.state)?;
    g.termination_reason = m
        .termination_reason
        .as_deref()
        .map(|r| from_wire::<TerminationReason>("termination_reason", r))
        .transpose()?;
    g.version = stored_int("version", m.version)?;
    Ok(g)
}

/// Binding columns: kind, subject type, its key, subject id, sub-grant id,
/// recipient tenant id.
type BindingColumns = (
    &'static str,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Uuid>,
    Option<Uuid>,
);

fn binding_columns(binding: &Binding) -> BindingColumns {
    match binding {
        Binding::Subject { subject, .. } => (
            "subject",
            Some(subject.gts_type.to_string()),
            Some(type_key(subject.gts_type.as_ref())),
            Some(subject.id.clone()),
            None,
            None,
        ),
        Binding::Delegated {
            grant_id,
            tenant_id,
            ..
        } => (
            "delegated",
            None,
            None,
            None,
            Some(grant_id.0),
            Some(*tenant_id),
        ),
        _ => ("none", None, None, None, None, None),
    }
}

fn utc(t: OffsetDateTime) -> OffsetDateTime {
    t.to_offset(time::UtcOffset::UTC)
}

fn to_active(g: &Grant, root: GrantId, now: OffsetDateTime) -> grant::ActiveModel {
    // SQLite keeps times as text and compares them as strings, which matches
    // the order of instants only in one offset.
    let w = g.validity.window();
    let window = &Window {
        not_before: utc(w.not_before),
        expires_at: utc(w.expires_at),
        grace_until: w.grace_until.map(utc),
    };
    let (kind, b_type, b_key, b_id, b_grant, b_tenant) = binding_columns(&g.binding);
    grant::ActiveModel {
        id: Set(g.id.0),
        holder_tenant_id: Set(g.holder_tenant_id),
        resource_type: Set(g.resource.gts_type.to_string()),
        resource_type_key: Set(type_key(g.resource.gts_type.as_ref())),
        resource_id: Set(g.resource.id.clone()),
        subject_type: Set(g.subject_type.to_string()),
        binding_kind: Set(kind.to_owned()),
        binding_subject_type: Set(b_type),
        binding_subject_type_key: Set(b_key),
        binding_subject_id: Set(b_id),
        binding_grant_id: Set(b_grant),
        binding_tenant_id: Set(b_tenant),
        ended_subject_type_key: Set(None),
        ended_subject_id: Set(None),
        binding_fixed: Set(g.binding_fixed),
        validity_kind: Set(match g.validity {
            Validity::Inherited { .. } => "inherited".to_owned(),
            _ => "own".to_owned(),
        }),
        not_before: Set(window.not_before),
        expires_at: Set(window.expires_at),
        grace_until: Set(window.grace_until),
        effective_end: Set(crate::domain::window::end(window)),
        conditions: Set(g.conditions.as_ref().map(|c| c.0.clone())),
        recipient_rules: Set(g.recipient_rules.as_ref().map(|c| c.0.clone())),
        may_bind: Set(g.permissions.may_bind),
        max_depth: Set(i32::from(g.permissions.max_depth)),
        requires_acceptance: Set(g.permissions.requires_acceptance),
        root_grant_id: Set(root.0),
        parent_grant_id: Set(g.parent_grant_id.map(|p| p.0)),
        grantor_tenant_id: Set(g.grantor_tenant_id),
        delegation_id: Set(g.delegation_id.map(|d| d.0)),
        pack_id: Set(g.pack_id.map(|p| p.0)),
        evidence_id: Set(g.evidence_id.0),
        state: Set(wire(&g.state)),
        termination_reason: Set(g.termination_reason.as_ref().map(wire)),
        version: Set(i64::try_from(g.version).unwrap_or(i64::MAX)),
        delegation_version: sea_orm::ActiveValue::NotSet,
        created_at: Set(now),
        updated_at: Set(now),
    }
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct IdAndPack {
    id: Uuid,
    pack_id: Option<Uuid>,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct ChildCount {
    parent_grant_id: Option<Uuid>,
    n: i64,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct IdOnly {
    id: Uuid,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct IdAndRoot {
    id: Uuid,
    root_grant_id: Uuid,
}

/// The evidence columns that the check needs when the plugin does not read
/// the bytes.
#[derive(Debug, sea_orm::FromQueryResult)]
struct EvidenceHeader {
    id: Uuid,
    plugin_id: String,
    intake: String,
}

/// `LIKE` pattern that matches `prefix` and every string that starts with it.
fn prefix_like(prefix: &str) -> LikeExpr {
    let escaped = prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    LikeExpr::new(format!("{escaped}%")).escape('\\')
}

fn filter_condition(f: &BindingFilter) -> Condition {
    Condition::all()
        .add(grant::Column::BindingKind.eq("subject"))
        .add(grant::Column::BindingSubjectId.eq(f.binding_id.clone()))
        .add(type_condition(grant::Column::BindingSubjectTypeKey, f))
}

/// `column`, a subject type key, matches the type pattern of `f`: the SQL form
/// of `filter_matches`.
fn type_condition(column: grant::Column, f: &BindingFilter) -> Condition {
    let key = type_key(f.type_pattern.as_ref());
    Condition::all().add(if f.exact {
        column.eq(key)
    } else {
        column.like(prefix_like(&key))
    })
}

fn resource_condition(resource_type: &GtsTypeId, resource_id: Option<&str>) -> Condition {
    let covers_id = match resource_id {
        Some(id) => Condition::any()
            .add(grant::Column::ResourceId.is_null())
            .add(grant::Column::ResourceId.eq(id)),
        None => Condition::all().add(grant::Column::ResourceId.is_null()),
    };
    Condition::all()
        .add(grant::Column::ResourceTypeKey.eq(type_key(resource_type.as_ref())))
        .add(covers_id)
}

/// `state` against a literal, not a parameter: PostgreSQL proves the
/// predicate of a partial index (`state <> 'terminated'`) only for a literal,
/// and a cached generic plan sees a parameter as any value.
fn state_is_terminated() -> SimpleExpr {
    Expr::col((grant::Entity, grant::Column::State)).eq(Expr::cust(format!("'{TERMINATED}'")))
}

fn state_is_not_terminated() -> SimpleExpr {
    Expr::col((grant::Entity, grant::Column::State)).ne(Expr::cust(format!("'{TERMINATED}'")))
}

fn live_at(instant: OffsetDateTime) -> Condition {
    Condition::all()
        .add(state_is_not_terminated())
        .add(grant::Column::EffectiveEnd.gt(utc(instant)))
}

/// When a grant stopped covering anything: its termination or the end of its
/// window.
fn ended_at(m: &grant::Model) -> OffsetDateTime {
    if m.state == TERMINATED {
        m.updated_at
    } else {
        m.effective_end
    }
}

/// The gear store.
#[derive(Clone, Copy)]
pub struct Store {
    limit_cfg: LimitCfg,
    max_candidates: u64,
}

impl Store {
    #[must_use]
    pub fn new(limit_cfg: LimitCfg, max_candidates: u64) -> Self {
        Self {
            limit_cfg,
            max_candidates,
        }
    }
}

#[async_trait]
impl GrantRepo for Store {
    /// A repeatable snapshot. `SQLite` reads one state in any transaction and
    /// takes no isolation level; read-only keeps its `BEGIN` deferred under
    /// `transaction_mode=immediate`, so a read does not queue behind writers.
    fn snapshot_read(&self, db: &DBProvider<DomainError>) -> TxConfig {
        let isolation =
            (db.db().db_engine() != "sqlite").then_some(TxIsolationLevel::RepeatableRead);
        TxConfig {
            isolation,
            access_mode: Some(TxAccessMode::ReadOnly),
        }
    }

    async fn candidates<C: DBRunner>(
        &self,
        runner: &C,
        tenant: Uuid,
        resource_type: &GtsTypeId,
        resource_id: Option<&str>,
        filters: &[BindingFilter],
        instant: OffsetDateTime,
    ) -> Result<Vec<Candidate>, DomainError> {
        // One query in candidate order: first the grants that are active and
        // whose window has started, then the rest, which can only give a reason
        // to deny; in each part by the first filter that matches, then by id.
        let mut any = Condition::any();
        let mut first_filter: Option<CaseStatement> = None;
        for (i, f) in filters.iter().enumerate() {
            let this = filter_condition(f);
            let i = i64::try_from(i).unwrap_or(i64::MAX);
            first_filter = Some(match first_filter {
                None => Expr::case(this.clone(), i),
                Some(rank) => rank.case(this.clone(), i),
            });
            any = any.add(this);
        }
        let Some(first_filter) = first_filter else {
            return Ok(Vec::new());
        };
        let started = Condition::all()
            .add(grant::Column::State.eq(wire(&GrantState::Active)))
            .add(grant::Column::NotBefore.lte(utc(instant)));
        let rows = grant::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant))
            .filter(resource_condition(resource_type, resource_id))
            .filter(live_at(instant))
            .filter(any)
            .order_by(
                Into::<Expr>::into(Expr::case(started, 0).finally(1)),
                Order::Asc,
            )
            .order_by(Into::<Expr>::into(first_filter), Order::Asc)
            .order_by(grant::Column::Id, Order::Asc)
            .limit(self.max_candidates)
            .all(runner)
            .await
            .map_err(db)?;
        if rows.len() as u64 == self.max_candidates {
            tracing::warn!(
                max_candidates = self.max_candidates,
                "check reached the candidate limit; later candidates are not read"
            );
        }
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let root = GrantId(row.root_grant_id);
            out.push(Candidate {
                grant: to_grant(row)?,
                root,
            });
        }
        Ok(out)
    }

    async fn roots<C: DBRunner>(
        &self,
        runner: &C,
        candidates: &[Candidate],
    ) -> Result<HashMap<GrantId, Grant>, DomainError> {
        let mut roots: HashMap<GrantId, Grant> = candidates
            .iter()
            .filter(|c| c.root == c.grant.id)
            .map(|c| (c.root, c.grant.clone()))
            .collect();
        let missing: Vec<Uuid> = candidates
            .iter()
            .filter(|c| !roots.contains_key(&c.root))
            .map(|c| c.root.0)
            .collect();
        if !missing.is_empty() {
            // SAFETY(cross-tenant read): the root of a sub-grant is held by an
            // ancestor tenant. This is the chain read of the check (DESIGN §3.7).
            let rows = grant::Entity::find()
                .secure()
                .scope_with(&AccessScope::allow_all())
                .filter(Condition::all().add(grant::Column::Id.is_in(missing)))
                .all(runner)
                .await
                .map_err(db)?;
            for row in rows {
                let g = to_grant(row)?;
                roots.insert(g.id, g);
            }
        }
        Ok(roots)
    }

    async fn any_live_on_resource<C: DBRunner>(
        &self,
        runner: &C,
        tenant: Uuid,
        resource_type: &GtsTypeId,
        resource_id: Option<&str>,
        instant: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        let found = grant::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant))
            .filter(resource_condition(resource_type, resource_id))
            .filter(live_at(instant))
            .limit(1)
            .one(runner)
            .await
            .map_err(db)?;
        Ok(found.is_some())
    }

    async fn last_ended<C: DBRunner>(
        &self,
        runner: &C,
        tenant: Uuid,
        resource_type: &GtsTypeId,
        resource_id: Option<&str>,
        filters: &[BindingFilter],
        instant: OffsetDateTime,
    ) -> Result<Option<Grant>, DomainError> {
        if filters.is_empty() {
            return Ok(None);
        }
        let any_filter = filters
            .iter()
            .fold(Condition::any(), |c, f| c.add(filter_condition(f)));
        // A termination that unbound the subject left it in the ended columns.
        // A query of its own, so that each one reads its index in order.
        let unbound_filter = filters.iter().fold(Condition::any(), |c, f| {
            c.add(
                Condition::all()
                    .add(grant::Column::EndedSubjectId.eq(f.binding_id.clone()))
                    .add(type_condition(grant::Column::EndedSubjectTypeKey, f)),
            )
        });
        let find = |subject: Condition| {
            grant::Entity::find()
                .secure()
                .scope_with(&AccessScope::for_tenant(tenant))
                .filter(resource_condition(resource_type, resource_id))
                .filter(subject)
        };
        let mut ended = Vec::with_capacity(3);
        for subject in [any_filter.clone(), unbound_filter] {
            ended.extend(
                find(subject)
                    .filter(Condition::all().add(state_is_terminated()))
                    .order_by(grant::Column::UpdatedAt, Order::Desc)
                    .order_by(grant::Column::Id, Order::Asc)
                    .limit(1)
                    .one(runner)
                    .await
                    .map_err(db)?,
            );
        }
        ended.extend(
            find(any_filter)
                .filter(
                    Condition::all()
                        .add(state_is_not_terminated())
                        .add(grant::Column::EffectiveEnd.lte(utc(instant))),
                )
                .order_by(grant::Column::EffectiveEnd, Order::Desc)
                .order_by(grant::Column::Id, Order::Asc)
                .limit(1)
                .one(runner)
                .await
                .map_err(db)?,
        );
        ended
            .into_iter()
            .max_by(|a, b| ended_at(a).cmp(&ended_at(b)).then(b.id.cmp(&a.id)))
            .map(to_grant)
            .transpose()
    }

    async fn evidence<C: DBRunner>(
        &self,
        runner: &C,
        ids: &[EvidenceId],
        with_bytes: bool,
    ) -> Result<HashMap<EvidenceId, StoredEvidence>, DomainError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // SAFETY(cross-tenant read): the evidence of roots that the check
        // already selected; their holder can be an ancestor tenant.
        let query = evidence::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(Condition::all().add(evidence::Column::Id.is_in(ids.iter().map(|id| id.0))));
        let rows: Vec<(Uuid, String, String, Vec<u8>)> = if with_bytes {
            query
                .all(runner)
                .await
                .map_err(db)?
                .into_iter()
                .map(|m| (m.id, m.plugin_id, m.intake, m.bytes))
                .collect()
        } else {
            query
                .project_all(runner, |q| {
                    q.select_only()
                        .column(evidence::Column::Id)
                        .column(evidence::Column::PluginId)
                        .column(evidence::Column::Intake)
                        .into_model::<EvidenceHeader>()
                })
                .await
                .map_err(db)?
                .into_iter()
                .map(|h| (h.id, h.plugin_id, h.intake, Vec::new()))
                .collect()
        };
        Ok(rows
            .into_iter()
            .map(|(id, plugin_id, intake, bytes)| {
                let id = EvidenceId(id);
                let intake = if intake == "import" {
                    Intake::Import
                } else {
                    Intake::Issue
                };
                (
                    id,
                    StoredEvidence {
                        id,
                        plugin_id,
                        intake,
                        bytes,
                    },
                )
            })
            .collect())
    }

    async fn material<C: DBRunner>(
        &self,
        runner: &C,
        plugin_id: &str,
        key: &str,
    ) -> Result<Option<Vec<u8>>, DomainError> {
        // Plugin material has no tenant: one store of the plugin for every
        // tenant, so `allow_all` crosses no tenant boundary.
        let row = plugin_material::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(plugin_material::Column::PluginId.eq(plugin_id))
                    .add(plugin_material::Column::Key.eq(key)),
            )
            .one(runner)
            .await
            .map_err(db)?;
        Ok(row.map(|r| r.value))
    }

    async fn apply_material<C: DBRunner>(
        &self,
        runner: &C,
        plugin_id: &str,
        writes: &[MaterialWrite],
    ) -> Result<bool, DomainError> {
        use toolkit_db::secure::SecureDeleteExt;
        // Plugin material has no tenant (see `material`).
        // Each write is one conditional statement, so a concurrent writer
        // cannot slip in between the expectation and the write.
        for w in writes {
            let key_cond = Condition::all()
                .add(plugin_material::Column::PluginId.eq(plugin_id))
                .add(plugin_material::Column::Key.eq(w.key.clone()));
            let held = match (&w.expect, &w.value) {
                (Expect::Equals(old), Some(value)) => {
                    plugin_material::Entity::update_many()
                        .col_expr(plugin_material::Column::Value, Expr::value(value.clone()))
                        .filter(key_cond.add(plugin_material::Column::Value.eq(old.clone())))
                        .secure()
                        .scope_with(&AccessScope::allow_all())
                        .exec(runner)
                        .await
                        .map_err(db)?
                        .rows_affected
                        == 1
                }
                (Expect::Equals(_) | Expect::Any, None) => {
                    let cond = match &w.expect {
                        Expect::Equals(old) => {
                            key_cond.add(plugin_material::Column::Value.eq(old.clone()))
                        }
                        _ => key_cond,
                    };
                    let deleted = plugin_material::Entity::delete_many()
                        .filter(cond)
                        .secure()
                        .scope_with(&AccessScope::allow_all())
                        .exec(runner)
                        .await
                        .map_err(db)?
                        .rows_affected;
                    matches!(w.expect, Expect::Any) || deleted == 1
                }
                (Expect::Absent | Expect::Any, Some(value)) => {
                    let am = plugin_material::ActiveModel {
                        plugin_id: Set(plugin_id.to_owned()),
                        key: Set(w.key.clone()),
                        value: Set(value.clone()),
                    };
                    let mut insert = plugin_material::Entity::insert(am)
                        .secure()
                        .scope_unchecked(&AccessScope::allow_all())
                        .map_err(db)?;
                    if matches!(w.expect, Expect::Any) {
                        insert = insert.on_conflict(
                            SecureOnConflict::<plugin_material::Entity>::columns([
                                plugin_material::Column::PluginId,
                                plugin_material::Column::Key,
                            ])
                            .update_columns([plugin_material::Column::Value])
                            .map_err(db)?,
                        );
                    }
                    match insert.exec(runner).await {
                        Ok(_) => true,
                        Err(e) if e.is_unique_violation() => false,
                        Err(e) => return Err(db(e)),
                    }
                }
                (Expect::Absent, None) => self.material(runner, plugin_id, &w.key).await?.is_none(),
                // An expectation that this gear does not know fails.
                _ => false,
            };
            if !held {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn idempotent_result<C: DBRunner>(
        &self,
        runner: &C,
        caller: Caller,
        token: &str,
        now: OffsetDateTime,
    ) -> Result<Option<(String, serde_json::Value)>, DomainError> {
        // SAFETY(own record): a record belongs to the tenant and subject of
        // its caller and is read by them alone.
        let row = idempotency::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(caller.tenant))
            .filter(
                Condition::all()
                    .add(idempotency::Column::SubjectId.eq(caller.subject))
                    .add(idempotency::Column::Token.eq(token))
                    .add(idempotency::Column::ExpiresAt.gt(now)),
            )
            .one(runner)
            .await
            .map_err(db)?;
        Ok(row.map(|r| (r.payload_hash, r.response)))
    }

    async fn record_result<C: DBRunner>(
        &self,
        runner: &C,
        caller: Caller,
        token: &str,
        payload_hash: String,
        response: serde_json::Value,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        use toolkit_db::secure::SecureDeleteExt;
        // SAFETY(own record): the caller writes its own record, after the PDP
        // allowed it the operation.
        let scope = AccessScope::for_tenant(caller.tenant);
        // Only an expired record gives way; a live one is a concurrent request
        // with the same token.
        idempotency::Entity::delete_many()
            .filter(
                Condition::all()
                    .add(idempotency::Column::SubjectId.eq(caller.subject))
                    .add(idempotency::Column::Token.eq(token))
                    .add(idempotency::Column::ExpiresAt.lte(utc(now))),
            )
            .secure()
            .scope_with(&scope)
            .exec(runner)
            .await
            .map_err(db)?;
        let am = idempotency::ActiveModel {
            tenant_id: Set(caller.tenant),
            subject_id: Set(caller.subject),
            token: Set(token.to_owned()),
            payload_hash: Set(payload_hash),
            response: Set(response),
            expires_at: Set(expires_at),
        };
        match secure_insert::<idempotency::Entity>(am, &scope, runner).await {
            Ok(_) => Ok(()),
            Err(e) if e.is_unique_violation() => Err(DomainError::IdempotencyConflict),
            Err(e) => Err(db(e)),
        }
    }

    async fn evidence_by_external_id<C: DBRunner>(
        &self,
        runner: &C,
        holder: Uuid,
        plugin_id: &str,
        external_id: &str,
    ) -> Result<Option<EvidenceId>, DomainError> {
        // SAFETY(own tenant): `holder` is the holder that the PDP allowed the
        // issue into; evidence belongs to it.
        let found = evidence::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(holder))
            .filter(
                Condition::all()
                    .add(evidence::Column::PluginId.eq(plugin_id))
                    .add(evidence::Column::ExternalId.eq(external_id)),
            )
            .one(runner)
            .await
            .map_err(db)?;
        Ok(found.map(|e| EvidenceId(e.id)))
    }

    async fn insert_evidence<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        row: NewEvidenceRow,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let am = evidence::ActiveModel {
            id: Set(row.id.0),
            holder_tenant_id: Set(row.holder),
            plugin_id: Set(row.plugin_id.to_owned()),
            intake: Set(match row.intake {
                Intake::Import => "import".to_owned(),
                _ => "issue".to_owned(),
            }),
            bytes: Set(row.bytes),
            replay_key: Set(row.replay_key),
            external_id: Set(row.external_id),
            replaces_evidence_id: Set(row.replaces.map(|e| e.0)),
            created_at: Set(now),
        };
        match secure_insert::<evidence::Entity>(am, scope, runner).await {
            Ok(_) => Ok(()),
            Err(ScopeError::Db(e)) => {
                Err(evidence_reuse(&e).unwrap_or_else(|| db(ScopeError::Db(e))))
            }
            Err(e) => Err(db(e)),
        }
    }

    async fn insert_pack<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        p: &Pack,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let am = pack::ActiveModel {
            id: Set(p.id.0),
            holder_tenant_id: Set(p.holder_tenant_id),
            version: Set(1),
            counts: Set(counts_json(&p.counts)?),
            created_at: Set(now),
        };
        secure_insert::<pack::Entity>(am, scope, runner)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn insert_roots<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        roots: &[Grant],
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        // Before the insert, to name the id: a failed statement aborts a
        // PostgreSQL transaction, so nothing can be read after it.
        let ids: Vec<GrantId> = roots.iter().map(|g| g.id).collect();
        // SAFETY(cross-tenant read): only whether these new ids exist anywhere.
        if let Some(g) = self
            .grants(runner, &AccessScope::allow_all(), &ids, false)
            .await?
            .first()
        {
            return Err(DomainError::RootAlreadyAdmitted(g.id));
        }
        let rows = roots.iter().map(|g| to_active(g, g.id, now)).collect();
        secure_insert_many::<grant::Entity>(rows, scope, runner)
            .await
            .map_err(db)
    }

    async fn grants<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        ids: &[GrantId],
        lock: bool,
    ) -> Result<Vec<Grant>, DomainError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut q = grant::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(grant::Column::Id.is_in(ids.iter().map(|id| id.0))))
            .order_by(grant::Column::Id, Order::Asc);
        if lock {
            q = q.lock_exclusive();
        }
        q.all(runner)
            .await
            .map_err(db)?
            .into_iter()
            .map(to_grant)
            .collect()
    }

    async fn grant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: GrantId,
        lock: bool,
    ) -> Result<Option<Grant>, DomainError> {
        let mut q = grant::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(grant::Column::Id.eq(id.0)));
        if lock {
            q = q.lock_exclusive();
        }
        q.one(runner).await.map_err(db)?.map(to_grant).transpose()
    }

    async fn grantor_grant<C: DBRunner>(
        &self,
        runner: &C,
        grantor: Uuid,
        id: GrantId,
        lock: bool,
    ) -> Result<Option<Grant>, DomainError> {
        // SAFETY(cross-tenant read): the named grantor query (DESIGN §3.2, Store).
        let mut q = grant::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(grant::Column::Id.eq(id.0))
                    .add(grant::Column::GrantorTenantId.eq(grantor)),
            );
        if lock {
            q = q.lock_exclusive();
        }
        q.one(runner).await.map_err(db)?.map(to_grant).transpose()
    }

    async fn insert_sub_grants<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        subs: &[(Grant, GrantId)],
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let rows = subs
            .iter()
            .map(|(g, root)| to_active(g, *root, now))
            .collect();
        match secure_insert_many::<grant::Entity>(rows, scope, runner).await {
            Ok(()) => Ok(()),
            Err(e) if e.is_unique_violation() => Err(DomainError::InvalidTransition(
                "the parent already has a live sub-grant".into(),
            )),
            Err(e) => Err(db(e)),
        }
    }

    async fn update_grants<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        grants: &[Grant],
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        // A pack member moves between the counts of its pack: read where each
        // was, before any write. Members are roots and never change their pack.
        let members: Vec<GrantId> = grants
            .iter()
            .filter(|g| g.pack_id.is_some() && g.parent_grant_id.is_none())
            .map(|g| g.id)
            .collect();
        let was: HashMap<Uuid, Option<PackCount>> = self
            .grants(runner, scope, &members, false)
            .await?
            .iter()
            .map(|m| (m.id.0, PackCount::of(m)))
            .collect();
        let mut moves: BTreeMap<Uuid, (Uuid, Vec<CountMove>)> = BTreeMap::new();
        update_rows(runner, scope, grants, now).await?;
        for g in grants {
            if let (Some(pack), Some(from)) = (g.pack_id, was.get(&g.id.0)) {
                let to = PackCount::of(g);
                if *from != to {
                    moves
                        .entry(pack.0)
                        .or_insert_with(|| (g.holder_tenant_id, Vec::new()))
                        .1
                        .push((g.resource.clone(), *from, to));
                }
            }
        }
        // In id order, as `lock_roots`.
        for (pack, (holder, moved)) in moves {
            move_counts(runner, PackId(pack), holder, &moved).await?;
        }
        Ok(())
    }

    async fn lock_roots<C: DBRunner>(
        &self,
        runner: &C,
        ids: &[GrantId],
    ) -> Result<(), DomainError> {
        // SAFETY(cross-tenant): a tree spans tenants; the caller decided
        // access on `ids`.
        let all = AccessScope::allow_all();
        let ids: Vec<Uuid> = ids.iter().map(|g| g.0).collect();
        let mut locked = BTreeSet::new();
        // A replacement can move a tree under a new root while this waits:
        // read the roots again after the lock until they stay.
        loop {
            let roots: BTreeSet<Uuid> = grant::Entity::find()
                .secure()
                .scope_with(&all)
                .filter(Condition::all().add(grant::Column::Id.is_in(ids.clone())))
                .project_all(runner, |q| {
                    q.select_only()
                        .column(grant::Column::Id)
                        .column(grant::Column::RootGrantId)
                        .into_model::<IdAndRoot>()
                })
                .await
                .map_err(db)?
                .into_iter()
                .map(|r| r.root_grant_id)
                .collect();
            let fresh: Vec<Uuid> = roots.difference(&locked).copied().collect();
            if fresh.is_empty() {
                break;
            }
            // A new root below a locked one breaks the id order, and a lock
            // cannot be given back inside the transaction: end it instead of
            // risking a deadlock.
            if fresh.first() < locked.last() {
                return Err(DomainError::TreeMoved);
            }
            grant::Entity::find()
                .secure()
                .scope_with(&all)
                .filter(Condition::all().add(grant::Column::Id.is_in(fresh.clone())))
                .order_by(grant::Column::Id, Order::Asc)
                .lock_exclusive()
                .project_all(runner, |q| {
                    q.select_only()
                        .column(grant::Column::Id)
                        .into_model::<IdOnly>()
                })
                .await
                .map_err(db)?;
            locked.extend(fresh);
        }
        // The pack of a member is locked by `move_counts`, only when its
        // counts change and always after the roots: roots first, packs
        // second, each in id order.
        Ok(())
    }

    async fn live_subtrees<C: DBRunner>(
        &self,
        runner: &C,
        ids: &[GrantId],
        lock: bool,
    ) -> Result<HashMap<GrantId, Vec<Grant>>, DomainError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // SAFETY(cross-tenant read): sub-grants are held by descendant tenants;
        // the caller decided access on `ids`.
        let all = AccessScope::allow_all();
        let live = || Condition::all().add(state_is_not_terminated());
        let roots: BTreeSet<Uuid> = grant::Entity::find()
            .secure()
            .scope_with(&all)
            .filter(live().add(grant::Column::Id.is_in(ids.iter().map(|id| id.0))))
            .project_all(runner, |q| {
                q.select_only()
                    .column(grant::Column::Id)
                    .column(grant::Column::RootGrantId)
                    .into_model::<IdAndRoot>()
            })
            .await
            .map_err(db)?
            .into_iter()
            .map(|r| r.root_grant_id)
            .collect();
        if roots.is_empty() {
            return Ok(HashMap::new());
        }
        // ponytail: reads and locks the whole live trees of the roots, siblings
        // included; a recursive query when trees grow large or contended.
        let mut q = grant::Entity::find()
            .secure()
            .scope_with(&all)
            .filter(live().add(grant::Column::RootGrantId.is_in(roots)))
            .order_by(grant::Column::Id, Order::Asc);
        if lock {
            q = q.lock_exclusive();
        }
        let rows = q.all(runner).await.map_err(db)?;
        let mut children: HashMap<Uuid, Vec<&grant::Model>> = HashMap::new();
        for row in &rows {
            if let Some(p) = row.parent_grant_id {
                children.entry(p).or_default().push(row);
            }
        }
        let mut out = HashMap::with_capacity(ids.len());
        for id in ids {
            // The locked read decides: the grant can have ended since the first one.
            let Some(top) = rows.iter().find(|r| r.id == id.0) else {
                continue;
            };
            // Breadth first: parents before children. A cycle in edited rows
            // ends at a row already taken.
            let mut seen = HashSet::from([top.id]);
            let mut subtree = vec![to_grant(top.clone())?];
            let mut i = 0;
            while i < subtree.len() {
                let parent = subtree[i].id.0;
                for child in children.get(&parent).into_iter().flatten() {
                    if seen.insert(child.id) {
                        subtree.push(to_grant((*child).clone())?);
                    }
                }
                i += 1;
            }
            out.insert(*id, subtree);
        }
        Ok(out)
    }

    async fn delegation_grants<C: DBRunner>(
        &self,
        runner: &C,
        party: Uuid,
        id: DelegationId,
        lock: bool,
    ) -> Result<(Vec<Grant>, u64), DomainError> {
        // SAFETY(cross-tenant read): a delegation spans the grantor and the
        // recipient; the rows are those where `party` is one of them.
        let mut q = grant::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(grant::Column::DelegationId.eq(id.0))
                    .add(
                        Condition::any()
                            .add(grant::Column::GrantorTenantId.eq(party))
                            .add(grant::Column::HolderTenantId.eq(party)),
                    ),
            )
            .order_by(grant::Column::Id, Order::Asc);
        if lock {
            q = q.lock_exclusive();
        }
        let rows = q.all(runner).await.map_err(db)?;
        let version = rows
            .iter()
            .map(|r| stored_int::<u64>("delegation_version", r.delegation_version))
            .try_fold(0_u64, |max, v| v.map(|v| Ord::max(max, v)))?;
        Ok((
            rows.into_iter().map(to_grant).collect::<Result<_, _>>()?,
            version,
        ))
    }

    async fn set_delegation_version<C: DBRunner>(
        &self,
        runner: &C,
        id: DelegationId,
        version: u64,
    ) -> Result<(), DomainError> {
        // SAFETY(cross-tenant write): the verb locked these rows as a party.
        grant::Entity::update_many()
            .col_expr(
                grant::Column::DelegationVersion,
                Expr::value(i64::try_from(version).unwrap_or(i64::MAX)),
            )
            .filter(Condition::all().add(grant::Column::DelegationId.eq(id.0)))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(runner)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn update_pack_version<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: PackId,
        version: u64,
    ) -> Result<(), DomainError> {
        let previous = i64::try_from(version.saturating_sub(1)).unwrap_or(i64::MAX);
        let result = pack::Entity::update_many()
            .col_expr(
                pack::Column::Version,
                Expr::value(i64::try_from(version).unwrap_or(i64::MAX)),
            )
            .filter(
                Condition::all()
                    .add(pack::Column::Id.eq(id.0))
                    .add(pack::Column::Version.eq(previous)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db)?;
        if result.rows_affected == 1 {
            Ok(())
        } else {
            Err(DomainError::VersionMismatch)
        }
    }

    async fn pack_grants<C: DBRunner>(
        &self,
        runner: &C,
        holder: Uuid,
        id: PackId,
        lock: bool,
    ) -> Result<Vec<Grant>, DomainError> {
        // SAFETY(own tenant): the members are roots of the holder of the pack;
        // the caller decided access on the pack and checks the members.
        let mut q = grant::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(holder))
            .filter(Condition::all().add(grant::Column::PackId.eq(id.0)))
            .order_by(grant::Column::Id, Order::Asc);
        if lock {
            q = q.lock_exclusive();
        }
        q.all(runner)
            .await
            .map_err(db)?
            .into_iter()
            .map(to_grant)
            .collect()
    }

    async fn chains_of<C: DBRunner>(
        &self,
        runner: &C,
        ids: &[GrantId],
    ) -> Result<HashMap<GrantId, Vec<Grant>>, DomainError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // SAFETY(cross-tenant read): the ancestors of a grant are held by
        // ancestor tenants; the caller decided access on `ids`.
        let starts: Vec<(GrantId, GrantId)> = grant::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(Condition::all().add(grant::Column::Id.is_in(ids.iter().map(|id| id.0))))
            .project_all(runner, |q| {
                q.select_only()
                    .column(grant::Column::Id)
                    .column(grant::Column::RootGrantId)
                    .into_model::<IdAndRoot>()
            })
            .await
            .map_err(db)?
            .into_iter()
            .map(|r| (GrantId(r.id), GrantId(r.root_grant_id)))
            .collect();
        walk_trees(runner, &starts).await
    }

    async fn chains<C: DBRunner>(
        &self,
        runner: &C,
        starts: &[(GrantId, GrantId)],
    ) -> Result<HashMap<GrantId, Vec<Grant>>, DomainError> {
        walk_trees(runner, starts).await
    }

    async fn live_children<C: DBRunner>(
        &self,
        runner: &C,
        parents: &[GrantId],
    ) -> Result<HashMap<GrantId, u64>, DomainError> {
        let mut counts: HashMap<GrantId, u64> = parents.iter().map(|p| (*p, 0)).collect();
        if parents.is_empty() {
            return Ok(counts);
        }
        // SAFETY(cross-tenant read): counts the chain links of the check.
        let rows = grant::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(grant::Column::ParentGrantId.is_in(parents.iter().map(|p| p.0)))
                    .add(state_is_not_terminated()),
            )
            .project_all(runner, |q| {
                q.select_only()
                    .column(grant::Column::ParentGrantId)
                    .column_as(Expr::col(grant::Column::Id).count(), "n")
                    .group_by(grant::Column::ParentGrantId)
                    .into_model::<ChildCount>()
            })
            .await
            .map_err(db)?;
        for r in rows {
            if let Some(n) = r.parent_grant_id.and_then(|p| counts.get_mut(&GrantId(p))) {
                *n = u64::try_from(r.n).unwrap_or(0);
            }
        }
        Ok(counts)
    }

    async fn live_rows<C: DBRunner>(
        &self,
        runner: &C,
        ids: &[GrantId],
    ) -> Result<HashMap<GrantId, u64>, DomainError> {
        let mut counts: HashMap<GrantId, u64> = ids.iter().map(|id| (*id, 0)).collect();
        if ids.is_empty() {
            return Ok(counts);
        }
        // SAFETY(cross-tenant read): counts the rows of the roots of the check.
        let rows = grant::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(grant::Column::Id.is_in(ids.iter().map(|id| id.0)))
                    .add(state_is_not_terminated()),
            )
            .project_all(runner, |q| {
                q.select_only()
                    .column(grant::Column::Id)
                    .into_model::<IdOnly>()
            })
            .await
            .map_err(db)?;
        for r in rows {
            if let Some(n) = counts.get_mut(&GrantId(r.id)) {
                *n += 1;
            }
        }
        Ok(counts)
    }

    async fn set_roots<C: DBRunner>(
        &self,
        runner: &C,
        roots: &[(GrantId, GrantId)],
    ) -> Result<(), DomainError> {
        for chunk in roots.chunks(UPDATE_CHUNK) {
            let mut case: Option<CaseStatement> = None;
            for (id, root) in chunk {
                let this = grant::Column::Id.eq(id.0);
                case = Some(match case.take() {
                    None => Expr::case(this, Expr::value(root.0)),
                    Some(c) => c.case(this, Expr::value(root.0)),
                });
            }
            let Some(case) = case else { continue };
            let column = grant::Column::RootGrantId;
            // SAFETY(cross-tenant write): replacement re-parents chains that
            // the vendor's replace addressed; their links are held by
            // descendant tenants.
            grant::Entity::update_many()
                .col_expr(column, case.finally(Expr::col(column)).into())
                .filter(
                    Condition::all().add(grant::Column::Id.is_in(chunk.iter().map(|(id, _)| id.0))),
                )
                .secure()
                .scope_with(&AccessScope::allow_all())
                .exec(runner)
                .await
                .map_err(db)?;
        }
        Ok(())
    }

    async fn list_packs<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        members: Option<&AccessScope>,
        query: &ODataQuery,
    ) -> Result<Page<Pack>, DomainError> {
        let mut base = pack::Entity::find().secure().scope_with(scope);
        let mut rest = query.clone();
        if let Some(expr) = rest.filter.take() {
            let invalid = |e: String| DomainError::Query(toolkit_odata::Error::InvalidFilter(e));
            let node = toolkit_odata::filter::convert_expr_to_filter_node::<PackFilterField>(&expr)
                .map_err(|e| invalid(e.to_string()))?;
            let matching = filter_node_to_condition::<PackFilterField, PackMemberMapper>(&node)
                .map_err(invalid)?;
            let members = members.ok_or_else(|| {
                DomainError::Internal("a pack filter without a grant scope".to_owned())
            })?;
            // The packs of the matching members that the grant scope admits.
            // SAFETY(raw subquery): `scope_with` applies the grant scope before
            // `into_inner`; after it come only the projection and filters that
            // narrow the rows. SecureORM has no scoped subquery yet.
            let ids = sea_orm::QueryTrait::into_query(
                grant::Entity::find()
                    .secure()
                    .scope_with(members)
                    .into_inner()
                    .select_only()
                    .column(grant::Column::PackId)
                    .filter(matching)
                    .filter(grant::Column::PackId.is_not_null()),
            );
            base = base.filter(Condition::all().add(pack::Column::Id.in_subquery(ids)));
        }
        let page = paginate_odata::<PackOrderField, PackODataMapper, _, _, _, _>(
            base,
            runner,
            &rest,
            ("id", SortDir::Desc),
            self.limit_cfg,
            |m| m,
        )
        .await
        .map_err(|e| match e {
            toolkit_odata::Error::Db(e) => DomainError::store(e),
            e => DomainError::Query(e),
        })?;
        let items = page
            .items
            .into_iter()
            .map(to_pack)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Page {
            items,
            page_info: page.page_info,
        })
    }

    async fn sweep_terminated<C: DBRunner>(
        &self,
        runner: &C,
        cutoff: OffsetDateTime,
        batch: u64,
    ) -> Result<u64, DomainError> {
        use toolkit_db::secure::SecureDeleteExt;
        // SAFETY(cross-tenant): retention is a store task for every tenant.
        let all = AccessScope::allow_all();
        // A terminated root ends its whole tree, and no write follows: its
        // `updated_at` is the last change of the tree.
        // A live row under a terminated root breaks the rule above: its tree
        // stays, and the batch moves past it.
        let eligible = || {
            let live = Alias::new("live");
            let live_row = Query::select()
                .expr(Expr::val(1))
                .from_as(grant::Entity, live.clone())
                .and_where(
                    Expr::col((live.clone(), grant::Column::RootGrantId))
                        .equals((grant::Entity, grant::Column::Id)),
                )
                .and_where(
                    Expr::col((live, grant::Column::State))
                        .ne(Expr::cust(format!("'{TERMINATED}'"))),
                )
                .to_owned();
            Condition::all()
                .add(grant::Column::ParentGrantId.is_null())
                .add(state_is_terminated())
                .add(grant::Column::UpdatedAt.lte(utc(cutoff)))
                .add(Expr::exists(live_row).not())
        };
        // The roots first, in id order, as `lock_roots` of every verb: a verb
        // in a tree holds its root, so no lock cycle forms.
        let rows = oldest_locked(runner, eligible, batch).await?;
        let branches = sweep_branches(runner, cutoff, batch).await?;
        if rows.is_empty() {
            return Ok(branches);
        }
        // Pack members are roots: the packs are those of the roots.
        let packs: BTreeSet<Uuid> = rows.iter().filter_map(|r| r.pack_id).collect();
        let roots: Vec<Uuid> = rows.into_iter().map(|r| r.id).collect();
        // ponytail: a root deletes its whole tree in one statement; the tree
        // holds at most one retention period of branches, since
        // `sweep_branches` takes the older ones. Chunk by rows if trees grow
        // large.
        let deleted = grant::Entity::delete_many()
            .filter(Condition::all().add(grant::Column::RootGrantId.is_in(roots)))
            .secure()
            .scope_with(&all)
            .exec(runner)
            .await
            .map_err(db)?
            .rows_affected
            + branches;
        if !packs.is_empty() {
            let member = Query::select()
                .expr(Expr::val(1))
                .from(grant::Entity)
                .and_where(
                    Expr::col((grant::Entity, grant::Column::PackId))
                        .equals((pack::Entity, pack::Column::Id)),
                )
                .to_owned();
            pack::Entity::delete_many()
                .filter(
                    Condition::all()
                        .add(pack::Column::Id.is_in(packs))
                        .add(Expr::exists(member).not()),
                )
                .secure()
                .scope_with(&all)
                .exec(runner)
                .await
                .map_err(db)?;
        }
        Ok(deleted)
    }

    async fn purge_idempotency<C: DBRunner>(
        &self,
        runner: &C,
        now: OffsetDateTime,
        batch: u64,
    ) -> Result<u64, DomainError> {
        use toolkit_db::secure::SecureDeleteExt;
        // SAFETY(cross-tenant): expiry is a store task for every tenant.
        let key = [
            idempotency::Column::TenantId,
            idempotency::Column::SubjectId,
            idempotency::Column::Token,
        ];
        let expired = Query::select()
            .columns(key)
            .from(idempotency::Entity)
            .and_where(Expr::col(idempotency::Column::ExpiresAt).lte(utc(now)))
            .limit(batch)
            .to_owned();
        Ok(idempotency::Entity::delete_many()
            .filter(Condition::all().add(Expr::tuple(key.map(Expr::col)).in_subquery(expired)))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(runner)
            .await
            .map_err(db)?
            .rows_affected)
    }

    async fn list_grants<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<Grant>, DomainError> {
        let base = grant::Entity::find().secure().scope_with(scope);
        let page = paginate_odata::<GrantFilterField, GrantODataMapper, _, _, _, _>(
            base,
            runner,
            query,
            ("id", SortDir::Desc),
            self.limit_cfg,
            |m| m,
        )
        .await
        .map_err(|e| match e {
            toolkit_odata::Error::Db(e) => DomainError::store(e),
            e => DomainError::Query(e),
        })?;
        let items = page
            .items
            .into_iter()
            .map(to_grant)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Page {
            items,
            page_info: page.page_info,
        })
    }

    async fn pack<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: PackId,
    ) -> Result<Option<Pack>, DomainError> {
        pack::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(pack::Column::Id.eq(id.0)))
            .one(runner)
            .await
            .map_err(db)?
            .map(to_pack)
            .transpose()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "repo_tests.rs"]
mod repo_tests;
