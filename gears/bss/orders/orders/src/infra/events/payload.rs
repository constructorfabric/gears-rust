//! The eleven Orders lifecycle event payloads (DESIGN §4.4 *The event set*, §4.7).
//!
//! An event's `data` is the bounded common order summary plus the concrete event's fields.
//! Expanded pins, descriptors and totals are never copied in (D-158): `orderId` and
//! `orderVersion` are the immutable read reference. Every free-text and list member is bounded
//! so the fully serialized envelope fits the 64 KiB outbox limit at the 200-line order cap.
use bss_orders_sdk::catalog::OrderState;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::borrow::Cow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::failure::FailureReason;

use super::{ORDER_SUBJECT_TYPE, SOURCE};

/// The declared line cap (DESIGN §3.3 working baseline); per-line event members never exceed it.
pub const MAX_LINES: usize = 200;
/// Caller-supplied reason text, stored as received (audit `caller_reason`, D-143).
pub const MAX_REASON_CHARS: usize = 4096;
/// Order external reference (`orders_order_admin.external_reference`).
pub const MAX_EXTERNAL_REFERENCE_CHARS: usize = 1024;
/// Opaque actor/authority references.
pub const MAX_REFERENCE_CHARS: usize = 256;
/// GTS order-category identifier.
pub const MAX_CATEGORY_CHARS: usize = 512;
/// ISO 8601 duration text of an expired state's TTL.
pub const MAX_DURATION_CHARS: usize = 64;

/// A violated event contract rule. Raised before enqueue, so the transition aborts.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Orders event contract violated: {0}")]
pub struct EventContractError(
    /// The violated rule.
    pub &'static str,
);

/// The common summary every Orders event carries (§4.4 *bounded common summary*).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderSummary {
    /// The order identifier; also the envelope subject.
    pub order_id: Uuid,
    /// The business version; the immutable `(orderId, orderVersion)` read reference.
    pub order_version: i32,
    /// The transition's business timestamp captured by Orders; not the envelope `occurred_at`.
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    /// Correlation identifier of the transition that emitted the event.
    pub correlation_id: Uuid,
    /// The GTS order-category identifier.
    pub category: String,
    /// The resulting state.
    pub state: OrderState,
    /// The order's resource tenant.
    pub resource_tenant_id: Uuid,
    /// The order's seller tenant.
    pub seller_tenant_id: Uuid,
    /// The order's payer tenant.
    pub payer_tenant_id: Uuid,
    /// The governing contract, when the order names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<Uuid>,
    /// The order's external reference, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_reference: Option<String>,
}

/// One concrete event's specific fields. Implemented only by the eleven types below.
pub trait EventKind:
    Serialize + DeserializeOwned + Clone + std::fmt::Debug + Send + Sync + 'static
{
    /// The concrete final GTS type identifier (§4.7).
    const TYPE_ID: &'static str;
    /// The closed catalog event this payload is: the compiler checks every payload names one.
    const KIND: bss_orders_sdk::catalog::EventKind;
    /// The PRD event name: the catalog token of [`Self::KIND`].
    const NAME: &'static str = Self::KIND.token();
    /// Reject a value its JSON schema or §4.4 row rules forbid.
    ///
    /// # Errors
    /// The first violated rule.
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError>;
}

/// A concrete Orders event: envelope tenancy plus `data` (summary and specific fields).
///
/// The envelope tenant is the platform-root tenant the producer binding resolved (D-95). It is
/// not part of `data`, and an event can only be built through [`super::TxEvents`], which stamps
/// it; no business tenant axis can reach it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderEvent<K> {
    #[serde(skip)]
    tenant: Uuid,
    /// The bounded common order summary.
    #[serde(flatten)]
    pub summary: OrderSummary,
    /// The concrete event's specific fields.
    #[serde(flatten)]
    pub detail: K,
}
impl<K: EventKind> OrderEvent<K> {
    pub(super) fn rooted(root: super::RootTenant, summary: OrderSummary, detail: K) -> Self {
        Self {
            tenant: root.0,
            summary,
            detail,
        }
    }

    /// Validate the summary and the concrete event's rules.
    ///
    /// # Errors
    /// The first violated rule.
    pub fn validate(&self) -> Result<(), EventContractError> {
        validate_summary(&self.summary)?;
        self.detail.validate(&self.summary)
    }
}
impl<K: EventKind> event_broker_sdk::TypedEvent for OrderEvent<K> {
    const TYPE_ID: &'static str = K::TYPE_ID;
    const SUBJECT_TYPE: &'static str = ORDER_SUBJECT_TYPE;
    const SOURCE: &'static str = SOURCE;
    /// The canonical hyphenated order UUID, equal to `data.orderId` (§4.4 envelope mapping).
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.summary.order_id.hyphenated().to_string())
    }
    /// Always the explicit platform-root tenant (D-95); never a business axis or the default.
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant)
    }
}

fn chars(value: &str) -> usize {
    value.chars().count()
}
fn bounded_text(value: &str, max: usize, rule: &'static str) -> Result<(), EventContractError> {
    if value.is_empty() || chars(value) > max {
        return Err(EventContractError(rule));
    }
    Ok(())
}
/// Caller text follows the audit `caller_reason` rule: no control characters except tab/CR/LF.
fn reason_text(value: &str, rule: &'static str) -> Result<(), EventContractError> {
    bounded_text(value, MAX_REASON_CHARS, rule)?;
    if value
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
    {
        return Err(EventContractError(rule));
    }
    Ok(())
}
/// Opaque references are printable ASCII (the platform event-field convention).
fn reference(value: &str, rule: &'static str) -> Result<(), EventContractError> {
    bounded_text(value, MAX_REFERENCE_CHARS, rule)?;
    if !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err(EventContractError(rule));
    }
    Ok(())
}
fn positive(version: i32, rule: &'static str) -> Result<(), EventContractError> {
    if version < 1 {
        return Err(EventContractError(rule));
    }
    Ok(())
}

fn validate_summary(summary: &OrderSummary) -> Result<(), EventContractError> {
    positive(summary.order_version, "orderVersion must be positive")?;
    for id in [
        summary.order_id,
        summary.correlation_id,
        summary.resource_tenant_id,
        summary.seller_tenant_id,
        summary.payer_tenant_id,
    ] {
        if id.is_nil() {
            return Err(EventContractError("summary identifiers must not be nil"));
        }
    }
    if summary.contract_id.is_some_and(|id| id.is_nil()) {
        return Err(EventContractError("contractId must not be nil"));
    }
    bounded_text(
        &summary.category,
        MAX_CATEGORY_CHARS,
        "category must be a bounded GTS category identifier",
    )?;
    if !summary
        .category
        .starts_with(toolkit_gts::gts_id!("cf.bss.orders.category.v1~"))
    {
        return Err(EventContractError(
            "category must be an Orders category identifier",
        ));
    }
    if let Some(reference) = &summary.external_reference {
        bounded_text(
            reference,
            MAX_EXTERNAL_REFERENCE_CHARS,
            "externalReference must be 1..1024 characters",
        )?;
    }
    Ok(())
}

fn expect_state(
    summary: &OrderSummary,
    allowed: &[OrderState],
    rule: &'static str,
) -> Result<(), EventContractError> {
    if allowed.contains(&summary.state) {
        Ok(())
    } else {
        Err(EventContractError(rule))
    }
}

/// One line of the committed version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineRef {
    /// The line's identity.
    pub line_id: Uuid,
}
/// Automatic acceptance written by the submit (05 §4.2, D-146).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitAcceptance {
    /// The accepted version; always the submitted version.
    pub accepted_version: i32,
    /// The acceptance instant.
    #[serde(with = "time::serde::rfc3339")]
    pub accepted_at: OffsetDateTime,
}
/// One completed line's subscription mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineSubscription {
    /// The completed line's identity.
    pub line_id: Uuid,
    /// The subscription created for the line.
    pub subscription_id: Uuid,
}

fn distinct_lines<'a>(
    ids: impl Iterator<Item = &'a Uuid>,
    count: usize,
    rule: &'static str,
) -> Result<(), EventContractError> {
    if count == 0 || count > MAX_LINES {
        return Err(EventContractError(rule));
    }
    let set: std::collections::BTreeSet<_> = ids.collect();
    if set.len() != count || set.iter().any(|id| id.is_nil()) {
        return Err(EventContractError(rule));
    }
    Ok(())
}

/// Row 4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderSubmitted {
    /// Identities of the submitted version's lines (1..200, distinct).
    pub lines: Vec<LineRef>,
    /// The automatic acceptance, when the submit wrote one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<SubmitAcceptance>,
}
impl EventKind for OrderSubmitted {
    const TYPE_ID: &'static str = concat_type!("submitted");
    const KIND: bss_orders_sdk::catalog::EventKind =
        bss_orders_sdk::catalog::EventKind::OrderSubmitted;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        expect_state(
            summary,
            &[OrderState::Submitted],
            "OrderSubmitted results in submitted",
        )?;
        distinct_lines(
            self.lines.iter().map(|l| &l.line_id),
            self.lines.len(),
            "OrderSubmitted carries 1..200 distinct line identities",
        )?;
        if let Some(acceptance) = &self.acceptance
            && acceptance.accepted_version != summary.order_version
        {
            return Err(EventContractError(
                "automatic acceptance names the submitted version",
            ));
        }
        Ok(())
    }
}

/// Rows 8 and 9.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderApproved {
    /// Reference of the authority that approved the order.
    pub deciding_authority: String,
    /// The approved version; always the current version.
    pub approved_version: i32,
}
impl EventKind for OrderApproved {
    const TYPE_ID: &'static str = concat_type!("approved");
    const KIND: bss_orders_sdk::catalog::EventKind =
        bss_orders_sdk::catalog::EventKind::OrderApproved;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        expect_state(
            summary,
            &[OrderState::Approved],
            "OrderApproved results in approved",
        )?;
        reference(
            &self.deciding_authority,
            "decidingAuthority must be a bounded reference",
        )?;
        positive(self.approved_version, "approvedVersion must be positive")?;
        if self.approved_version != summary.order_version {
            return Err(EventContractError("approvedVersion is the current version"));
        }
        Ok(())
    }
}

/// Row 10.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderRejected {
    /// Reference of the authority that rejected the order.
    pub deciding_authority: String,
    /// The opaque received `denial_reason` (D-135).
    pub denial_reason: String,
}
impl EventKind for OrderRejected {
    const TYPE_ID: &'static str = concat_type!("rejected");
    const KIND: bss_orders_sdk::catalog::EventKind =
        bss_orders_sdk::catalog::EventKind::OrderRejected;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        expect_state(
            summary,
            &[OrderState::Rejected],
            "OrderRejected results in rejected",
        )?;
        reference(
            &self.deciding_authority,
            "decidingAuthority must be a bounded reference",
        )?;
        reason_text(&self.denial_reason, "denialReason must be bounded text")
    }
}

/// Rows 18, 19 and 20.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderAmended {
    /// The version this amendment supersedes; lower than the new `orderVersion`.
    pub supersedes_version: i32,
}
impl EventKind for OrderAmended {
    const TYPE_ID: &'static str = concat_type!("amended");
    const KIND: bss_orders_sdk::catalog::EventKind =
        bss_orders_sdk::catalog::EventKind::OrderAmended;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        // Rows 18-20 all end in `submitted` (01 §4.3).
        expect_state(
            summary,
            &[OrderState::Submitted],
            "OrderAmended results in submitted",
        )?;
        positive(
            self.supersedes_version,
            "supersedesVersion must be positive",
        )?;
        if self.supersedes_version >= summary.order_version {
            return Err(EventContractError(
                "supersedesVersion precedes the new orderVersion",
            ));
        }
        Ok(())
    }
}

/// Row 21.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderHeld {
    /// The outgoing state.
    pub previous_state: OrderState,
    /// Present only when the caller supplied one (D-138).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_reason: Option<String>,
}
impl EventKind for OrderHeld {
    const TYPE_ID: &'static str = concat_type!("held");
    const KIND: bss_orders_sdk::catalog::EventKind = bss_orders_sdk::catalog::EventKind::OrderHeld;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        expect_state(
            summary,
            &[OrderState::OnHold],
            "OrderHeld results in on_hold",
        )?;
        if !is_holdable(self.previous_state) {
            return Err(EventContractError(
                "OrderHeld leaves a row-21 state: submitted, pending_approval, approved or in_fulfillment",
            ));
        }
        if let Some(reason) = &self.hold_reason {
            reason_text(reason, "holdReason must be bounded text")?;
        }
        Ok(())
    }
}

/// Row 22.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderResumed {
    /// The restored pre-hold state; equals the resulting state.
    pub restored_state: OrderState,
}
impl EventKind for OrderResumed {
    const TYPE_ID: &'static str = concat_type!("resumed");
    const KIND: bss_orders_sdk::catalog::EventKind =
        bss_orders_sdk::catalog::EventKind::OrderResumed;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        // Row 22 restores the stored pre-hold state, which row 21 admits only from a holdable state.
        if summary.state != self.restored_state || !is_holdable(self.restored_state) {
            return Err(EventContractError(
                "OrderResumed results in its live restored state",
            ));
        }
        Ok(())
    }
}

/// Rows 5, 15, 16, 17, 23 and 27.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderCancelled {
    /// Reference of the actor that cancelled the order.
    pub cancelling_actor: String,
    /// Mandatory, carried from the committed audit `caller_reason` (D-143).
    pub cancel_reason: String,
    /// Present where the cancel was workflow-mediated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compensation_evidence: Option<CompensationEvidence>,
}
impl EventKind for OrderCancelled {
    const TYPE_ID: &'static str = concat_type!("cancelled");
    const KIND: bss_orders_sdk::catalog::EventKind =
        bss_orders_sdk::catalog::EventKind::OrderCancelled;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        expect_state(
            summary,
            &[OrderState::Cancelled],
            "OrderCancelled results in cancelled",
        )?;
        reference(
            &self.cancelling_actor,
            "cancellingActor must be a bounded reference",
        )?;
        reason_text(&self.cancel_reason, "cancelReason must be bounded text")?;
        if let Some(evidence) = &self.compensation_evidence {
            evidence.validate()?;
            if evidence.is_forced() || !evidence.no_active_subscription_remains.is_true() {
                return Err(EventContractError(
                    "a workflow-mediated cancel asserts no active subscription remains",
                ));
            }
        }
        Ok(())
    }
}

/// Rows 6 and 24.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderExpired {
    /// The TTL-governed state the order expired from.
    pub expired_state: OrderState,
    /// The effective TTL as an ISO 8601 duration.
    pub ttl: String,
    /// The effective TTL policy's identifier.
    pub ttl_policy_id: Uuid,
    /// The effective TTL policy's revision.
    pub ttl_policy_revision: i64,
    /// The platform default policy's revision at expiry.
    pub platform_policy_revision: i64,
}
impl EventKind for OrderExpired {
    const TYPE_ID: &'static str = concat_type!("expired");
    const KIND: bss_orders_sdk::catalog::EventKind =
        bss_orders_sdk::catalog::EventKind::OrderExpired;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        expect_state(
            summary,
            &[OrderState::Expired],
            "OrderExpired results in expired",
        )?;
        if !matches!(
            self.expired_state,
            OrderState::Draft
                | OrderState::Submitted
                | OrderState::PendingApproval
                | OrderState::Approved
                | OrderState::OnHold
        ) {
            return Err(EventContractError("expiredState is a TTL-governed state"));
        }
        bounded_text(
            &self.ttl,
            MAX_DURATION_CHARS,
            "ttl must be a bounded duration",
        )?;
        if !self.ttl.starts_with('P') || !self.ttl.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(EventContractError("ttl must be an ISO 8601 duration"));
        }
        if self.ttl_policy_id.is_nil()
            || self.ttl_policy_revision < 1
            || self.platform_policy_revision < 1
        {
            return Err(EventContractError(
                "the effective policy identity and revisions are required",
            ));
        }
        Ok(())
    }
}

/// Row 13.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderCompleted {
    /// Each completed line's subscription (1..200, distinct).
    pub lines: Vec<LineSubscription>,
}
impl EventKind for OrderCompleted {
    const TYPE_ID: &'static str = concat_type!("completed");
    const KIND: bss_orders_sdk::catalog::EventKind =
        bss_orders_sdk::catalog::EventKind::OrderCompleted;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        expect_state(
            summary,
            &[OrderState::Completed],
            "OrderCompleted results in completed",
        )?;
        distinct_lines(
            self.lines.iter().map(|l| &l.line_id),
            self.lines.len(),
            "OrderCompleted maps 1..200 distinct lines",
        )?;
        distinct_lines(
            self.lines.iter().map(|l| &l.subscription_id),
            self.lines.len(),
            "OrderCompleted subscription identifiers are distinct",
        )
    }
}

/// Rows 14, 26, 28 and 29.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderFulfillmentFailed {
    /// Why fulfillment failed.
    pub failure_reason: FailureReason,
    /// The operator's forced-failure reason; only with `operator-forced-unreconciled` (D-182).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_reason: Option<String>,
    /// Workflow's compensation evidence; its forced variant on rows 28 and 29.
    pub compensation_evidence: CompensationEvidence,
}
impl EventKind for OrderFulfillmentFailed {
    const TYPE_ID: &'static str = concat_type!("fulfillment_failed");
    const KIND: bss_orders_sdk::catalog::EventKind =
        bss_orders_sdk::catalog::EventKind::OrderFulfillmentFailed;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        expect_state(
            summary,
            &[OrderState::FulfillmentFailed],
            "OrderFulfillmentFailed results in fulfillment_failed",
        )?;
        self.compensation_evidence.validate()?;
        let forced = matches!(
            self.failure_reason,
            FailureReason::OperatorForcedUnreconciled
        );
        match (&self.forced_reason, forced) {
            (Some(reason), true) => reason_text(reason, "forcedReason must be bounded text")?,
            (None, false) => {}
            _ => {
                return Err(EventContractError(
                    "forcedReason is present exactly with operator-forced-unreconciled",
                ));
            }
        }
        if self.compensation_evidence.is_forced() != forced {
            return Err(EventContractError(
                "only an operator-forced failure carries forced compensation evidence",
            ));
        }
        if !forced
            && !self
                .compensation_evidence
                .no_active_subscription_remains
                .is_true()
        {
            return Err(EventContractError(
                "a reported failure asserts no active subscription remains",
            ));
        }
        Ok(())
    }
}

/// The acceptance requirement source (05 §4.2; `orders_acceptance.requirement_source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementSource {
    /// A contract requires acceptance.
    Contract,
    /// A seller-scope election requires acceptance.
    Seller,
    /// The platform-scope election, or its unset fallback, requires acceptance.
    PlatformDefault,
    /// Acceptance was recorded although none was required.
    Volunteered,
}

/// Row 25.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderAcceptanceRecorded {
    /// The accepted version; always the current post-draft version.
    pub accepted_version: i32,
    /// The acceptance instant.
    #[serde(with = "time::serde::rfc3339")]
    pub accepted_at: OffsetDateTime,
    /// Reference of the actor that recorded the acceptance.
    pub recording_actor: String,
    /// Where the acceptance requirement came from.
    pub requirement_source: RequirementSource,
}
impl EventKind for OrderAcceptanceRecorded {
    const TYPE_ID: &'static str = concat_type!("acceptance_recorded");
    const KIND: bss_orders_sdk::catalog::EventKind =
        bss_orders_sdk::catalog::EventKind::OrderAcceptanceRecorded;
    fn validate(&self, summary: &OrderSummary) -> Result<(), EventContractError> {
        expect_state(
            summary,
            &[
                OrderState::Submitted,
                OrderState::PendingApproval,
                OrderState::Approved,
                OrderState::InFulfillment,
                OrderState::OnHold,
            ],
            "OrderAcceptanceRecorded keeps a live accepted state",
        )?;
        // Row 25 records acceptance of the current immutable version (`accepted_version =
        // expected_version` under the final version check, 05 §3.6), never version 1, the
        // draft materialization (`orders_acceptance.accepted_version > 1`).
        if self.accepted_version < 2 || self.accepted_version != summary.order_version {
            return Err(EventContractError(
                "acceptedVersion is the current committed post-draft version",
            ));
        }
        reference(
            &self.recording_actor,
            "recordingActor must be a bounded reference",
        )
    }
}

/// The row-21 hold sources, and so the only states row 22 can restore (01 §4.3).
pub const HOLDABLE_STATES: [OrderState; 4] = [
    OrderState::Submitted,
    OrderState::PendingApproval,
    OrderState::Approved,
    OrderState::InFulfillment,
];
fn is_holdable(state: OrderState) -> bool {
    HOLDABLE_STATES.contains(&state)
}

/// A boolean assertion or, in the forced variant only, `unknown` (D-182).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Assertion {
    /// A definite boolean assertion.
    Known(bool),
    /// The literal `unknown`; forced variant only.
    Unknown(UnknownToken),
}
impl Assertion {
    fn is_true(self) -> bool {
        matches!(self, Self::Known(true))
    }
}
/// The literal `"unknown"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnknownToken {
    /// Serialized as `"unknown"`.
    Unknown,
}

/// Two-operator attestation of a forced unreconciled failure (D-182).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorAttestation {
    /// The requesting operator's actor reference.
    pub requested_by: Uuid,
    /// Audit entry of the requester's refused attempt.
    pub request_audit_id: Uuid,
    /// When the requester made the refused attempt.
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
    /// The approving operator's actor reference; distinct from the requester.
    pub approved_by: Uuid,
}

/// Compensation evidence under the closed schema of DESIGN §3.7 (`orders_order`), with the
/// exact stored member names; Lifecycle validates structure only (06 §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompensationEvidence {
    /// Subscription drafts Workflow voided.
    pub drafts_voided: Vec<Uuid>,
    /// Activated subscriptions Workflow rolled back.
    pub activated_rolled_back: Vec<Uuid>,
    /// Whether any activation intent was dispatched.
    pub activation_dispatched: bool,
    /// Whether at-sale billable facts had been emitted.
    pub at_sale_facts_emitted: Assertion,
    /// Workflow's assertion that no active subscription remains.
    pub no_active_subscription_remains: Assertion,
    /// The two-operator attestation; present only in the forced variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_attestation: Option<OperatorAttestation>,
}
impl CompensationEvidence {
    fn is_forced(&self) -> bool {
        self.operator_attestation.is_some()
    }
    /// The same closed rules as `bss_orders__compensation_valid`.
    fn validate(&self) -> Result<(), EventContractError> {
        if self.drafts_voided.len() > MAX_LINES || self.activated_rolled_back.len() > MAX_LINES {
            return Err(EventContractError(
                "compensation lists are bounded by the line cap",
            ));
        }
        match &self.operator_attestation {
            None => {
                if matches!(self.at_sale_facts_emitted, Assertion::Unknown(_))
                    || matches!(self.no_active_subscription_remains, Assertion::Unknown(_))
                {
                    return Err(EventContractError(
                        "unknown assertions belong to the forced variant only",
                    ));
                }
            }
            Some(attestation) => {
                if !self.activation_dispatched
                    || !matches!(self.at_sale_facts_emitted, Assertion::Unknown(_))
                    || !matches!(self.no_active_subscription_remains, Assertion::Unknown(_))
                {
                    return Err(EventContractError(
                        "the forced variant asserts dispatch and unknown outcomes",
                    ));
                }
                if attestation.requested_by == attestation.approved_by {
                    return Err(EventContractError(
                        "a forced failure needs two distinct operators",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Every concrete event type identifier, in §4.4 table order.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the event contract tests compare the registered GTS types against it; no runtime \
                  path needs the list"
    )
)]
pub const EVENT_TYPE_IDS: [&str; 11] = [
    OrderSubmitted::TYPE_ID,
    OrderApproved::TYPE_ID,
    OrderRejected::TYPE_ID,
    OrderAmended::TYPE_ID,
    OrderHeld::TYPE_ID,
    OrderResumed::TYPE_ID,
    OrderCancelled::TYPE_ID,
    OrderExpired::TYPE_ID,
    OrderCompleted::TYPE_ID,
    OrderFulfillmentFailed::TYPE_ID,
    OrderAcceptanceRecorded::TYPE_ID,
];
