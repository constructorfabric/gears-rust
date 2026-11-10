//! Queries of the gear store. Every query goes through the Secure ORM.

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use gts::GtsTypeId;
use license_enforcement_sdk::dto::GrantFilterField;
use license_enforcement_sdk::models::{
    Binding, BindingCounts, Conditions, Grant, GrantState, Pack, Permissions, ResourceRef,
    SubjectRef, TerminationReason, Validity, Window,
};
use license_enforcement_sdk::{
    BindingFilter, EvidenceId, Expect, GrantId, Intake, MaterialWrite, PackId, StoredEvidence,
};
use sea_orm::sea_query::{CaseStatement, Expr, LikeExpr};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, QueryFilter, QuerySelect, Set};
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureOnConflict, SecureUpdateExt,
    secure_insert,
};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::entity::{evidence, grant, idempotency, pack, plugin_material};
use super::odata::GrantODataMapper;
use crate::domain::error::DomainError;
use crate::domain::repo::{Candidate, GrantRepo, NewEvidenceRow};
use crate::domain::types::type_key;

const TERMINATED: &str = "terminated";

fn db(e: ScopeError) -> DomainError {
    match e {
        ScopeError::TenantNotInScope { .. } | ScopeError::Denied(_) => {
            DomainError::PermissionDenied
        }
        other => DomainError::store(other),
    }
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

fn to_grant(m: grant::Model) -> Result<Grant, DomainError> {
    let binding = match m.binding_kind.as_str() {
        "none" => Binding::None,
        "subject" => Binding::Subject {
            subject: SubjectRef {
                gts_type: GtsTypeId::new(m.binding_subject_type.as_deref().unwrap_or_default()),
                id: m.binding_subject_id.unwrap_or_default(),
            },
        },
        "delegated" => Binding::Delegated {
            grant_id: GrantId(m.binding_grant_id.unwrap_or_default()),
        },
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
    Ok(Grant {
        id: GrantId(m.id),
        holder_tenant_id: m.holder_tenant_id,
        resource: ResourceRef {
            gts_type: GtsTypeId::new(&m.resource_type),
            id: m.resource_id,
        },
        subject_type: GtsTypeId::new(&m.subject_type),
        binding,
        binding_fixed: m.binding_fixed,
        validity,
        conditions: m.conditions.map(Conditions),
        permissions: Permissions {
            may_bind: m.may_bind,
            max_depth: u16::try_from(m.max_depth).unwrap_or(0),
            requires_acceptance: m.requires_acceptance,
        },
        recipient_rules: m.recipient_rules.map(Conditions),
        parent_grant_id: m.parent_grant_id.map(GrantId),
        delegation_id: m.delegation_id.map(Into::into),
        pack_id: m.pack_id.map(PackId),
        evidence_id: EvidenceId(m.evidence_id),
        state: from_wire("state", &m.state)?,
        termination_reason: m
            .termination_reason
            .as_deref()
            .map(|r| from_wire::<TerminationReason>("termination_reason", r))
            .transpose()?,
        version: u64::try_from(m.version).unwrap_or(0),
    })
}

/// Binding columns: kind, subject type, its key, subject id, sub-grant id.
type BindingColumns = (
    &'static str,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Uuid>,
);

fn binding_columns(binding: &Binding) -> BindingColumns {
    match binding {
        Binding::Subject { subject } => (
            "subject",
            Some(subject.gts_type.to_string()),
            Some(type_key(subject.gts_type.as_ref())),
            Some(subject.id.clone()),
            None,
        ),
        Binding::Delegated { grant_id } => ("delegated", None, None, None, Some(grant_id.0)),
        _ => ("none", None, None, None, None),
    }
}

fn utc(t: OffsetDateTime) -> OffsetDateTime {
    t.to_offset(time::UtcOffset::UTC)
}

fn effective_end(window: &Window) -> OffsetDateTime {
    window.grace_until.unwrap_or(window.expires_at)
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
    let (kind, b_type, b_key, b_id, b_grant) = binding_columns(&g.binding);
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
        binding_fixed: Set(g.binding_fixed),
        validity_kind: Set(match g.validity {
            Validity::Inherited { .. } => "inherited".to_owned(),
            _ => "own".to_owned(),
        }),
        not_before: Set(window.not_before),
        expires_at: Set(window.expires_at),
        grace_until: Set(window.grace_until),
        effective_end: Set(effective_end(window)),
        conditions: Set(g.conditions.as_ref().map(|c| c.0.clone())),
        recipient_rules: Set(g.recipient_rules.as_ref().map(|c| c.0.clone())),
        may_bind: Set(g.permissions.may_bind),
        max_depth: Set(i32::from(g.permissions.max_depth)),
        requires_acceptance: Set(g.permissions.requires_acceptance),
        root_grant_id: Set(root.0),
        parent_grant_id: Set(g.parent_grant_id.map(|p| p.0)),
        delegation_id: Set(g.delegation_id.map(|d| d.0)),
        pack_id: Set(g.pack_id.map(|p| p.0)),
        evidence_id: Set(g.evidence_id.0),
        state: Set(wire(&g.state)),
        termination_reason: Set(g.termination_reason.as_ref().map(wire)),
        version: Set(i64::try_from(g.version).unwrap_or(i64::MAX)),
        created_at: Set(now),
        updated_at: Set(now),
    }
}

/// The evidence columns that the check needs when the plugin does not read
/// the bytes.
#[derive(Debug, sea_orm::FromQueryResult)]
struct EvidenceHeader {
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
    let key = type_key(f.type_pattern.as_ref());
    Condition::all()
        .add(grant::Column::BindingKind.eq("subject"))
        .add(grant::Column::BindingSubjectId.eq(f.binding_id.clone()))
        .add(if f.exact {
            grant::Column::BindingSubjectTypeKey.eq(key)
        } else {
            grant::Column::BindingSubjectTypeKey.like(prefix_like(&key))
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

fn live_at(instant: OffsetDateTime) -> Condition {
    Condition::all()
        .add(grant::Column::State.ne(TERMINATED))
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
        let find = || {
            grant::Entity::find()
                .secure()
                .scope_with(&AccessScope::for_tenant(tenant))
                .filter(resource_condition(resource_type, resource_id))
                .filter(any_filter.clone())
        };
        let terminated = find()
            .filter(Condition::all().add(grant::Column::State.eq(TERMINATED)))
            .order_by(grant::Column::UpdatedAt, Order::Desc)
            .order_by(grant::Column::Id, Order::Asc)
            .limit(1)
            .one(runner)
            .await
            .map_err(db)?;
        let expired = find()
            .filter(
                Condition::all()
                    .add(grant::Column::State.ne(TERMINATED))
                    .add(grant::Column::EffectiveEnd.lte(utc(instant))),
            )
            .order_by(grant::Column::EffectiveEnd, Order::Desc)
            .order_by(grant::Column::Id, Order::Asc)
            .limit(1)
            .one(runner)
            .await
            .map_err(db)?;
        terminated
            .into_iter()
            .chain(expired)
            .max_by(|a, b| ended_at(a).cmp(&ended_at(b)).then(b.id.cmp(&a.id)))
            .map(to_grant)
            .transpose()
    }

    async fn evidence<C: DBRunner>(
        &self,
        runner: &C,
        id: EvidenceId,
        with_bytes: bool,
    ) -> Result<StoredEvidence, DomainError> {
        // SAFETY(cross-tenant read): the evidence of a root that the check
        // already selected; its holder can be an ancestor tenant.
        let query = evidence::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(Condition::all().add(evidence::Column::Id.eq(id.0)));
        let row = if with_bytes {
            query
                .one(runner)
                .await
                .map_err(db)?
                .map(|m| (m.plugin_id, m.intake, m.bytes))
        } else {
            query
                .project_all(runner, |q| {
                    q.select_only()
                        .column(evidence::Column::PluginId)
                        .column(evidence::Column::Intake)
                        .into_model::<EvidenceHeader>()
                })
                .await
                .map_err(db)?
                .pop()
                .map(|h| (h.plugin_id, h.intake, Vec::new()))
        };
        let (plugin_id, intake, bytes) =
            row.ok_or_else(|| DomainError::Internal(format!("evidence {id} is missing")))?;
        Ok(StoredEvidence {
            id,
            plugin_id,
            intake: if intake == "import" {
                Intake::Import
            } else {
                Intake::Issue
            },
            bytes,
        })
    }

    async fn material<C: DBRunner>(
        &self,
        runner: &C,
        plugin_id: &str,
        key: &str,
    ) -> Result<Option<Vec<u8>>, DomainError> {
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
        tenant: Uuid,
        token: &str,
        now: OffsetDateTime,
    ) -> Result<Option<(String, serde_json::Value)>, DomainError> {
        let row = idempotency::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant))
            .filter(
                Condition::all()
                    .add(idempotency::Column::TenantId.eq(tenant))
                    .add(idempotency::Column::Token.eq(token))
                    .add(idempotency::Column::ExpiresAt.gt(now)),
            )
            .one(runner)
            .await
            .map_err(db)?;
        Ok(row.map(|r| (r.payload_hash, r.response)))
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_result<C: DBRunner>(
        &self,
        runner: &C,
        tenant: Uuid,
        token: &str,
        payload_hash: String,
        response: serde_json::Value,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        use toolkit_db::secure::SecureDeleteExt;
        let scope = AccessScope::for_tenant(tenant);
        // Only an expired record gives way; a live one is a concurrent request
        // with the same token.
        idempotency::Entity::delete_many()
            .filter(
                Condition::all()
                    .add(idempotency::Column::TenantId.eq(tenant))
                    .add(idempotency::Column::Token.eq(token))
                    .add(idempotency::Column::ExpiresAt.lte(utc(now))),
            )
            .secure()
            .scope_with(&scope)
            .exec(runner)
            .await
            .map_err(db)?;
        let am = idempotency::ActiveModel {
            tenant_id: Set(tenant),
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

    async fn replay_key_used<C: DBRunner>(
        &self,
        runner: &C,
        plugin_id: &str,
        replay_key: &str,
    ) -> Result<bool, DomainError> {
        let found = evidence::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(evidence::Column::PluginId.eq(plugin_id))
                    .add(evidence::Column::ReplayKey.eq(replay_key)),
            )
            .one(runner)
            .await
            .map_err(db)?;
        Ok(found.is_some())
    }

    async fn evidence_by_external_id<C: DBRunner>(
        &self,
        runner: &C,
        plugin_id: &str,
        external_id: &str,
    ) -> Result<Option<(EvidenceId, bool)>, DomainError> {
        let Some(found) = evidence::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(evidence::Column::PluginId.eq(plugin_id))
                    .add(evidence::Column::ExternalId.eq(external_id)),
            )
            .one(runner)
            .await
            .map_err(db)?
        else {
            return Ok(None);
        };
        let successor = evidence::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(evidence::Column::PluginId.eq(plugin_id))
                    .add(evidence::Column::ReplacesEvidenceId.eq(found.id)),
            )
            .one(runner)
            .await
            .map_err(db)?;
        Ok(Some((EvidenceId(found.id), successor.is_some())))
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
            Err(e) if e.is_unique_violation() => Err(DomainError::AdmissionConflict),
            Err(e) => Err(db(e)),
        }
    }

    async fn insert_pack<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: PackId,
        holder: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let am = pack::ActiveModel {
            id: Set(id.0),
            holder_tenant_id: Set(holder),
            version: Set(1),
            created_at: Set(now),
        };
        secure_insert::<pack::Entity>(am, scope, runner)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn insert_root<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        g: &Grant,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        match secure_insert::<grant::Entity>(to_active(g, g.id, now), scope, runner).await {
            Ok(_) => Ok(()),
            Err(e) if e.is_unique_violation() => Err(DomainError::RootAlreadyAdmitted(g.id)),
            Err(e) => Err(db(e)),
        }
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

    async fn update_binding<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        g: &Grant,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let (kind, b_type, b_key, b_id, b_grant) = binding_columns(&g.binding);
        let previous = i64::try_from(g.version.saturating_sub(1)).unwrap_or(i64::MAX);
        let result = grant::Entity::update_many()
            .col_expr(grant::Column::BindingKind, Expr::value(kind))
            .col_expr(grant::Column::BindingSubjectType, Expr::value(b_type))
            .col_expr(grant::Column::BindingSubjectTypeKey, Expr::value(b_key))
            .col_expr(grant::Column::BindingSubjectId, Expr::value(b_id))
            .col_expr(grant::Column::BindingGrantId, Expr::value(b_grant))
            .col_expr(
                grant::Column::Version,
                Expr::value(i64::try_from(g.version).unwrap_or(i64::MAX)),
            )
            .col_expr(grant::Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(grant::Column::Id.eq(g.id.0))
                    .add(grant::Column::Version.eq(previous)),
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
        let Some(row) = pack::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(pack::Column::Id.eq(id.0)))
            .one(runner)
            .await
            .map_err(db)?
        else {
            return Ok(None);
        };
        // ponytail: counts in memory over the live members; a GROUP BY when packs
        // grow to many thousands of members.
        // Access was decided on the pack row; the scope built for a pack does not
        // fit the grant table. The members are held by the pack's holder.
        let members = grant::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(row.holder_tenant_id))
            .filter(
                Condition::all()
                    .add(grant::Column::PackId.eq(id.0))
                    .add(grant::Column::State.ne(TERMINATED)),
            )
            .all(runner)
            .await
            .map_err(db)?;
        let mut counts: BTreeMap<(String, Option<String>), BindingCounts> = BTreeMap::new();
        for m in members {
            let entry = counts
                .entry((m.resource_type.clone(), m.resource_id.clone()))
                .or_insert_with(|| BindingCounts {
                    resource: ResourceRef {
                        gts_type: GtsTypeId::new(&m.resource_type),
                        id: m.resource_id.clone(),
                    },
                    unbound: 0,
                    bound: 0,
                    delegated: 0,
                });
            match m.binding_kind.as_str() {
                "subject" => entry.bound += 1,
                "delegated" => entry.delegated += 1,
                _ => entry.unbound += 1,
            }
        }
        Ok(Some(Pack {
            id,
            holder_tenant_id: row.holder_tenant_id,
            counts: counts.into_values().collect(),
            version: u64::try_from(row.version).unwrap_or(0),
        }))
    }
}
