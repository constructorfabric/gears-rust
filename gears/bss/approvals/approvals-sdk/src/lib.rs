//! The source port one BSS gear registers so the approvals inbox can read and vote
//! on that gear's units as the caller.
//!
//! A gear registers it with
//! `ctx.client_hub().register_scoped::<dyn ApprovalSourceV1>(ClientScope::new(SOURCE), ..)`.
//! `SOURCE` is the gear's stable name (`pricing`, `products`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// One gear's approval units, read and voted on as the caller, by calling the gear's own door
/// functions. The inbox never copies those doors.
#[async_trait]
pub trait ApprovalSourceV1: Send + Sync {
    /// Up to `q.limit` units strictly after `q.after` in `q.order`, keyset on `(submitted_at, id)`.
    ///
    /// # Errors
    /// The gear's own door error. A kind outside the gear's closed set, and `book_id` on a gear
    /// that has no book, are an empty page, not an error.
    async fn page(
        &self,
        ctx: &SecurityContext,
        q: &SourcePageQuery,
    ) -> Result<SourcePage, CanonicalError>;

    /// The counts under the narrowing, by state and by kind.
    ///
    /// # Errors
    /// The gear's own counts-door error. A kind outside the gear's closed set, and `book_id` on a
    /// gear that has no book, are zero counts, not an error.
    async fn counts(
        &self,
        ctx: &SecurityContext,
        n: &SourceNarrowing,
    ) -> Result<SourceCounts, CanonicalError>;

    /// One unit. `Ok(None)` when this gear holds no unit with this id that the caller's tenant
    /// can see.
    ///
    /// # Errors
    /// The gear's own card-door error, other than a miss inside the tenant.
    async fn get(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        impact: bool,
    ) -> Result<Option<InboxUnit>, CanonicalError>;

    /// The actors this source's gear records that are not people: its own system actors, and any
    /// other gear's that act on it. The inbox names them "System" and never asks Account
    /// Management for them (AP-D-11). None by default.
    fn system_actors(&self) -> &[Uuid] {
        &[]
    }

    /// The gear's vote door, passed through: the request body bytes and the idempotency key as
    /// received, the door's answer (status, headers, body) unchanged.
    ///
    /// # Errors
    /// The source could not call its door. A door refusal is [`VoteResponse`], not an error.
    async fn vote(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        action: VoteAction,
        request: VoteRequest,
    ) -> Result<VoteResponse, CanonicalError>;
}

/// Which vote door the inbox routes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoteAction {
    /// `POST …/approve`.
    Approve,
    /// `POST …/reject`.
    Reject,
    /// `POST …/withdraw`.
    Withdraw,
}

impl VoteAction {
    /// The path segment of the owning gear's vote door.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Withdraw => "withdraw",
        }
    }
}

/// The vote body and the caller's idempotency key, as the inbox received them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteRequest {
    /// The request body bytes, unmodified.
    pub body: Vec<u8>,
    /// The `Idempotency-Key` header, when the caller sent one.
    pub idempotency_key: Option<String>,
}

/// The owning door's HTTP answer, unmodified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteResponse {
    /// HTTP status.
    pub status: u16,
    /// Response headers, in the order the door set them.
    pub headers: Vec<(String, String)>,
    /// Response body bytes.
    pub body: Vec<u8>,
}

/// Keyset direction. The unit id breaks a tie in the same direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    /// Oldest `submitted_at` first.
    Asc,
    /// Newest `submitted_at` first.
    Desc,
}

/// One position in the `(submitted_at, id)` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortKey {
    /// When the unit was submitted. Compared as an instant.
    #[serde(with = "time::serde::rfc3339")]
    pub submitted_at: OffsetDateTime,
    /// The unit id. Its byte order is the lower-case hex order.
    pub id: Uuid,
}

impl SortKey {
    /// The key of a unit the inbox has already shown.
    #[must_use]
    pub const fn of(unit: &InboxUnit) -> Self {
        Self {
            submitted_at: unit.submitted_at,
            id: unit.id,
        }
    }
}

/// The list's narrowing. The source decides which values are empty and which are a refusal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceNarrowing {
    /// `pending`, `approved`, `rejected`, or `withdrawn`.
    pub state: Option<String>,
    /// One name from the union closed set, or a name only one gear records.
    pub kind: Option<String>,
    /// The referenced aggregate.
    pub ref_id: Option<Uuid>,
    /// On pricing, the alias of `ref_id` (prices of that book, no plan revision). On products, empty.
    pub book_id: Option<Uuid>,
}

/// One page asked of one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePageQuery {
    /// Which units the page keeps.
    pub narrowing: SourceNarrowing,
    /// `submitted_at` direction. The id breaks a tie the same way.
    pub order: Order,
    /// At most this many units.
    pub limit: u32,
    /// The caller's last taken key for this source. `None` reads from the start.
    pub after: Option<SortKey>,
    /// When false, the source skips its live impact read and answers `impact: null`.
    pub impact: bool,
}

/// The units one source returned, and whether that source has more after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePage {
    /// Units strictly after the asked key, in the asked order, at most `limit`.
    pub units: Vec<InboxUnit>,
    /// The source has a further unit after `units`.
    pub has_more: bool,
}

/// Counts under one narrowing. Every state and every union kind is named, `0` when none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCounts {
    /// Units in each state.
    pub by_state: StateCounts,
    /// Units of each kind the inbox knows.
    pub by_kind: KindCounts,
    /// Units the narrowing keeps.
    pub total: u64,
}

impl SourceCounts {
    /// Adds another source's counts into this one.
    pub fn add(&mut self, other: &Self) {
        self.by_state.add(&other.by_state);
        self.by_kind.add(&other.by_kind);
        self.total = self.total.saturating_add(other.total);
    }
}

/// Units in each state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateCounts {
    /// `pending`.
    pub pending: u64,
    /// `approved`.
    pub approved: u64,
    /// `rejected`.
    pub rejected: u64,
    /// `withdrawn`.
    pub withdrawn: u64,
}

impl StateCounts {
    /// Adds another source's state counts into this one.
    pub fn add(&mut self, other: &Self) {
        self.pending = self.pending.saturating_add(other.pending);
        self.approved = self.approved.saturating_add(other.approved);
        self.rejected = self.rejected.saturating_add(other.rejected);
        self.withdrawn = self.withdrawn.saturating_add(other.withdrawn);
    }
}

/// Units of each kind the inbox's closed set names. A gear's counts door names only its own kinds;
/// read from it, the others are `0`. A kind field outside this set does not decode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KindCounts {
    /// Pricing: a price submission.
    pub prices: u64,
    /// Pricing: a plan revision.
    pub plan_revision: u64,
    /// Products: a SKU publish.
    pub sku_publish: u64,
    /// Products: a SKU change.
    pub sku_change: u64,
    /// Products: a SKU retire.
    pub sku_retire: u64,
}

impl KindCounts {
    /// Adds another source's kind counts into this one.
    pub fn add(&mut self, other: &Self) {
        self.prices = self.prices.saturating_add(other.prices);
        self.plan_revision = self.plan_revision.saturating_add(other.plan_revision);
        self.sku_publish = self.sku_publish.saturating_add(other.sku_publish);
        self.sku_change = self.sku_change.saturating_add(other.sku_change);
        self.sku_retire = self.sku_retire.saturating_add(other.sku_retire);
    }
}

/// The kinds the inbox merges. A gear records a subset and treats the rest as empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxKind {
    /// Pricing records this.
    Prices,
    /// Pricing records this.
    PlanRevision,
    /// Products records this.
    SkuPublish,
    /// Products records this.
    SkuChange,
    /// Products records this.
    SkuRetire,
}

impl InboxKind {
    /// The stored and served kind name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prices => "prices",
            Self::PlanRevision => "plan_revision",
            Self::SkuPublish => "sku_publish",
            Self::SkuChange => "sku_change",
            Self::SkuRetire => "sku_retire",
        }
    }
}

/// The unit's state. The same four names in every gear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitState {
    /// Waiting for votes.
    Pending,
    /// Quorum reached.
    Approved,
    /// A reviewer rejected it.
    Rejected,
    /// The submitter withdrew it.
    Withdrawn,
}

impl UnitState {
    /// The stored and served state name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Withdrawn => "withdrawn",
        }
    }

    /// Parses a stored name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "withdrawn" => Some(Self::Withdrawn),
            _ => None,
        }
    }
}

/// One reviewer's vote on a generation of a unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxDecision {
    /// Who voted.
    pub actor: Uuid,
    /// The generation the vote belongs to.
    pub generation: i32,
    /// `approve` or `reject`.
    pub decision: DecisionKind,
    /// The voter's note.
    pub note: Option<String>,
    /// When the vote was stored.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The vote is from an earlier generation.
    pub stale: bool,
}

/// A vote stored as `approve` or `reject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    /// An approve vote.
    Approve,
    /// A reject vote.
    Reject,
}

/// One approval unit as the inbox serves it. Concurrency is `generation`; there is no `version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the wire carries three independent caller flags (AP-D-7)"
)]
pub struct InboxUnit {
    /// The unit id.
    pub id: Uuid,
    /// The gear that holds it (`pricing`, `products`).
    pub source: String,
    /// One of the union closed set.
    pub kind: InboxKind,
    /// The referenced aggregate's type.
    pub ref_type: String,
    /// The referenced aggregate's id.
    pub ref_id: Uuid,
    /// The unit's state.
    pub state: UnitState,
    /// The generation a vote must name.
    pub generation: i32,
    /// Approves required to apply the unit.
    pub quorum_required: u32,
    /// `YYYY-MM-DD`, when the unit names a common effective date.
    pub common_effective_date: Option<String>,
    /// Who submitted the unit.
    pub submitted_by: Uuid,
    /// When the unit was submitted. Compared as an instant.
    #[serde(with = "time::serde::rfc3339")]
    pub submitted_at: OffsetDateTime,
    /// The submitter's note.
    pub submit_note: Option<String>,
    /// When the unit reached a terminal state.
    #[serde(with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
    /// The deciding note.
    pub decided_note: Option<String>,
    /// The snapshot the reviewers see.
    pub snapshot: serde_json::Value,
    /// Decisions of every generation.
    pub decisions: Vec<InboxDecision>,
    /// Whether the caller may approve the unit now. The owning gear judges it.
    pub caller_can_approve: bool,
    /// Whether the caller may reject the unit now. The owning gear judges it.
    #[serde(default)]
    pub caller_can_reject: bool,
    /// Whether the caller may withdraw the unit now. The owning gear judges it.
    #[serde(default)]
    pub caller_can_withdraw: bool,
    /// Products: the card's live SKU head. Pricing: null.
    pub subject_live: Option<serde_json::Value>,
    /// The live impact, or null when it was skipped or the source could not read it.
    pub impact: Option<serde_json::Value>,
}

impl InboxUnit {
    /// The unit one of the gear's own doors served, as the inbox serves it.
    ///
    /// `unit` is the door's JSON unit, the shape the BSS gears share (products P-D-219): the
    /// inbox reads the door's bytes, never a copy of its rules. The door's `impact_live` (the
    /// products card's live SKU head) becomes `subject_live`; a field the door does not serve
    /// (`subject_live` on pricing, `impact` on products) is null, for the source to fill.
    ///
    /// # Errors
    /// The door's unit does not read as an inbox unit: a kind or a state outside the closed sets,
    /// or a field of another type.
    pub fn from_door(source: &str, mut unit: serde_json::Value) -> Result<Self, serde_json::Error> {
        let Some(fields) = unit.as_object_mut() else {
            return Err(serde::de::Error::custom("a door's unit is a JSON object"));
        };
        let live = fields
            .remove("impact_live")
            .unwrap_or(serde_json::Value::Null);
        fields.insert("source".to_owned(), source.into());
        fields.entry("subject_live").or_insert(live);
        fields.entry("impact").or_insert(serde_json::Value::Null);
        serde_json::from_value(unit)
    }
}

#[cfg(test)]
mod kind_counts_tests {
    use super::KindCounts;

    #[test]
    fn a_missing_kind_is_zero_and_an_unknown_kind_does_not_decode() {
        let counts: KindCounts = serde_json::from_str(r#"{"sku_retire":4}"#).expect("known kind");
        assert_eq!(counts.sku_retire, 4);
        assert_eq!(counts.prices, 0);
        assert_eq!(counts.plan_revision, 0);
        assert!(
            serde_json::from_str::<KindCounts>(r#"{"prices":1,"other":2}"#).is_err(),
            "an unknown kind must not decode"
        );
    }
}
