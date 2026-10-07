//! Wire shapes. They follow the inbox unit; the vote answer is the owning door's bytes.

use bss_approvals_sdk::{
    DecisionKind, InboxDecision, InboxKind, InboxUnit, KindCounts, SourceCounts, StateCounts,
    UnitState,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::read::{SourceHealth, SourceRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum InboxKindDto {
    #[serde(rename = "prices")]
    Prices,
    #[serde(rename = "plan_revision")]
    PlanRevision,
    #[serde(rename = "sku_publish")]
    SkuPublish,
    #[serde(rename = "sku_change")]
    SkuChange,
    #[serde(rename = "sku_retire")]
    SkuRetire,
}

impl From<InboxKind> for InboxKindDto {
    fn from(kind: InboxKind) -> Self {
        match kind {
            InboxKind::Prices => Self::Prices,
            InboxKind::PlanRevision => Self::PlanRevision,
            InboxKind::SkuPublish => Self::SkuPublish,
            InboxKind::SkuChange => Self::SkuChange,
            InboxKind::SkuRetire => Self::SkuRetire,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum InboxStateDto {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "approved")]
    Approved,
    #[serde(rename = "rejected")]
    Rejected,
    #[serde(rename = "withdrawn")]
    Withdrawn,
}

impl From<UnitState> for InboxStateDto {
    fn from(state: UnitState) -> Self {
        match state {
            UnitState::Pending => Self::Pending,
            UnitState::Approved => Self::Approved,
            UnitState::Rejected => Self::Rejected,
            UnitState::Withdrawn => Self::Withdrawn,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum InboxDecisionKindDto {
    #[serde(rename = "approve")]
    Approve,
    #[serde(rename = "reject")]
    Reject,
}

impl From<DecisionKind> for InboxDecisionKindDto {
    fn from(kind: DecisionKind) -> Self {
        match kind {
            DecisionKind::Approve => Self::Approve,
            DecisionKind::Reject => Self::Reject,
        }
    }
}

#[toolkit_macros::api_dto(response)]
pub struct InboxDecisionDto {
    pub actor: Uuid,
    /// The current name of `actor` (AP-D-11): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights. Null when no name is
    /// available now.
    pub actor_name: Option<String>,
    pub generation: i32,
    pub decision: InboxDecisionKindDto,
    pub note: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub at: OffsetDateTime,
    pub stale: bool,
}

impl From<InboxDecision> for InboxDecisionDto {
    fn from(decision: InboxDecision) -> Self {
        Self {
            actor: decision.actor,
            actor_name: None,
            generation: decision.generation,
            decision: decision.decision.into(),
            note: decision.note,
            at: decision.at,
            stale: decision.stale,
        }
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "the wire carries three independent caller flags (AP-D-7)"
)]
#[toolkit_macros::api_dto(response)]
pub struct InboxUnitDto {
    pub id: Uuid,
    pub source: String,
    pub kind: InboxKindDto,
    pub ref_type: String,
    pub ref_id: Uuid,
    pub state: InboxStateDto,
    pub generation: i32,
    pub quorum_required: u32,
    pub common_effective_date: Option<String>,
    pub submitted_by: Uuid,
    /// The current name of `submitted_by` (AP-D-11): its display name, else first and last name, else
    /// username, from Account Management under the caller's rights. Null when no name is
    /// available now.
    pub submitted_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub submitted_at: OffsetDateTime,
    pub submit_note: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub decided_at: Option<OffsetDateTime>,
    pub decided_note: Option<String>,
    pub snapshot: serde_json::Value,
    pub decisions: Vec<InboxDecisionDto>,
    /// Whether the caller may approve this unit now. The owning gear judges the engine's rule
    /// and the caller's approve grant (AP-D-7).
    pub caller_can_approve: bool,
    /// Whether the caller may reject this unit now. The owning gear judges the grant, that the
    /// unit is pending, and that the caller has not voted in this generation (AP-D-7).
    pub caller_can_reject: bool,
    /// Whether the caller may withdraw this unit now. The owning gear judges the submitter, that
    /// the unit is pending, and the submit grant (AP-D-7).
    pub caller_can_withdraw: bool,
    pub subject_live: Option<serde_json::Value>,
    pub impact: Option<serde_json::Value>,
}

impl From<InboxUnit> for InboxUnitDto {
    fn from(unit: InboxUnit) -> Self {
        Self {
            id: unit.id,
            source: unit.source,
            kind: unit.kind.into(),
            ref_type: unit.ref_type,
            ref_id: unit.ref_id,
            state: unit.state.into(),
            generation: unit.generation,
            quorum_required: unit.quorum_required,
            common_effective_date: unit.common_effective_date,
            submitted_by: unit.submitted_by,
            submitted_by_name: None,
            submitted_at: unit.submitted_at,
            submit_note: unit.submit_note,
            decided_at: unit.decided_at,
            decided_note: unit.decided_note,
            snapshot: unit.snapshot,
            decisions: unit.decisions.into_iter().map(Into::into).collect(),
            caller_can_approve: unit.caller_can_approve,
            caller_can_reject: unit.caller_can_reject,
            caller_can_withdraw: unit.caller_can_withdraw,
            subject_live: unit.subject_live,
            impact: unit.impact,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum InboxSourceStatusDto {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "forbidden")]
    Forbidden,
    #[serde(rename = "unavailable")]
    Unavailable,
}

impl From<SourceHealth> for InboxSourceStatusDto {
    fn from(status: SourceHealth) -> Self {
        match status {
            SourceHealth::Ok => Self::Ok,
            SourceHealth::Forbidden => Self::Forbidden,
            SourceHealth::Unavailable => Self::Unavailable,
        }
    }
}

#[toolkit_macros::api_dto(response)]
pub struct InboxSourceDto {
    pub name: String,
    pub status: InboxSourceStatusDto,
}

impl From<SourceRow> for InboxSourceDto {
    fn from(row: SourceRow) -> Self {
        Self {
            name: row.name,
            status: row.status.into(),
        }
    }
}

#[toolkit_macros::api_dto(response)]
pub struct InboxUnitListDto {
    pub items: Vec<InboxUnitDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub sources: Vec<InboxSourceDto>,
}

#[toolkit_macros::api_dto(response)]
pub struct InboxStateCountsDto {
    pub pending: u64,
    pub approved: u64,
    pub rejected: u64,
    pub withdrawn: u64,
}

impl From<StateCounts> for InboxStateCountsDto {
    fn from(counts: StateCounts) -> Self {
        Self {
            pending: counts.pending,
            approved: counts.approved,
            rejected: counts.rejected,
            withdrawn: counts.withdrawn,
        }
    }
}

#[toolkit_macros::api_dto(response)]
pub struct InboxKindCountsDto {
    pub prices: u64,
    pub plan_revision: u64,
    pub sku_publish: u64,
    pub sku_change: u64,
    pub sku_retire: u64,
}

impl From<KindCounts> for InboxKindCountsDto {
    fn from(counts: KindCounts) -> Self {
        Self {
            prices: counts.prices,
            plan_revision: counts.plan_revision,
            sku_publish: counts.sku_publish,
            sku_change: counts.sku_change,
            sku_retire: counts.sku_retire,
        }
    }
}

#[toolkit_macros::api_dto(response)]
pub struct InboxCountsDto {
    pub by_state: InboxStateCountsDto,
    pub by_kind: InboxKindCountsDto,
    pub total: u64,
    pub sources: Vec<InboxSourceDto>,
}

impl InboxCountsDto {
    #[must_use]
    pub fn from_counts(counts: SourceCounts, sources: Vec<SourceRow>) -> Self {
        Self {
            by_state: counts.by_state.into(),
            by_kind: counts.by_kind.into(),
            total: counts.total,
            sources: sources.into_iter().map(Into::into).collect(),
        }
    }
}

bss_rest::actor_fields!(InboxDecisionDto { actor => actor_name } []);
bss_rest::actor_fields!(InboxUnitListDto {}[items]);

/// The actor ids a live subject may carry, each with the `*_name` key that its gear's answer
/// puts beside it (a products SKU: its creator, and its archiver while archived).
const LIVE_ACTORS: [(&str, &str); 2] = [
    ("created_by", "created_by_name"),
    ("archived_by", "archived_by_name"),
];

/// A unit names its submitter and its voters (AP-D-11), and each actor of its gear's live subject
/// that carries a `*_name` key beside it (a products SKU's creator and archiver). The inbox is the
/// only one that names a unit it serves: its source answers it unnamed.
impl bss_rest::actor_names::ActorFields for InboxUnitDto {
    fn actor_ids(&self, ids: &mut Vec<Uuid>) {
        ids.push(self.submitted_by);
        self.decisions.actor_ids(ids);
        for (field, name) in LIVE_ACTORS {
            ids.extend(live_actor(self.subject_live.as_ref(), field, name));
        }
    }
    fn fill_names(&mut self, names: &bss_rest::actor_names::Names) {
        self.submitted_by_name = bss_rest::actor_names::label(names, self.submitted_by);
        self.decisions.fill_names(names);
        for (field, name) in LIVE_ACTORS {
            if let Some(id) = live_actor(self.subject_live.as_ref(), field, name)
                && let Some(serde_json::Value::Object(live)) = self.subject_live.as_mut()
            {
                live.insert(
                    name.to_owned(),
                    bss_rest::actor_names::label(names, id)
                        .map_or(serde_json::Value::Null, serde_json::Value::String),
                );
            }
        }
    }
}

/// The actor in `field` of a live subject that carries the `name` key beside it.
fn live_actor(live: Option<&serde_json::Value>, field: &str, name: &str) -> Option<Uuid> {
    let live = live?;
    live.get(name)?;
    live.get(field)?.as_str()?.parse().ok()
}
