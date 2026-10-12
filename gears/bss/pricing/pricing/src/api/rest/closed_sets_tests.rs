#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;

/// How a set reads a stored token back.
type Stored<T> = fn(&str, &dyn std::fmt::Display) -> Result<T, RepoError>;

/// Every value's wire token is its stored token and reads back as itself, and the schema's `enum`
/// lists exactly those tokens in order (serde and the schema generator read a rename apart: a
/// variant whose own snake case is not its token would publish the wrong one).
fn round_trips<T>(all: &[T], as_str: fn(T) -> &'static str, stored: Stored<T>) -> Vec<&'static str>
where
    T: Copy + PartialEq + std::fmt::Debug + serde::Serialize + utoipa::PartialSchema,
{
    let tokens: Vec<&'static str> = all
        .iter()
        .map(|&v| {
            assert_eq!(serde_json::to_value(v).unwrap(), as_str(v), "{v:?}");
            assert_eq!(stored(as_str(v), &"row").unwrap(), v);
            as_str(v)
        })
        .collect();
    let schema = serde_json::to_value(T::schema()).unwrap();
    assert_eq!(schema["type"], "string", "{schema}");
    assert_eq!(schema["enum"], serde_json::json!(tokens), "{schema}");
    tokens
}

#[test]
fn change_kind_is_set_cancel_or_end() {
    macro_rules! tokens {
        ($t:ident) => {
            round_trips($t::ALL, $t::as_str, $t::stored)
        };
    }
    assert_eq!(tokens!(PricingChangeKind), ["set", "cancel", "end"]);
}
#[test]
fn each_set_carries_its_stored_tokens_on_the_wire() {
    macro_rules! tokens {
        ($t:ident) => {
            round_trips($t::ALL, $t::as_str, $t::stored)
        };
    }
    assert_eq!(
        tokens!(PricingChargeKind),
        ["recurring", "usage", "one_time"]
    );
    assert_eq!(tokens!(PricingPeriod), ["month", "year"]);
    assert_eq!(
        tokens!(PricingModel),
        ["flat", "per_unit", "graduated", "volume", "package"]
    );
    assert_eq!(
        tokens!(PricingEntryReferenceState),
        ["confirmation_pending", "confirmed", "lost", "released"]
    );
    assert_eq!(tokens!(PricingEligibility), ["all", "new"]);
    assert_eq!(
        tokens!(PricingPriceState),
        ["draft", "pending", "approved", "rejected", "cancelled"]
    );
    assert_eq!(
        tokens!(PricingPriceStatus),
        [
            "draft",
            "pending",
            "rejected",
            "scheduled",
            "active",
            "superseded",
            "cancelled"
        ]
    );
    assert_eq!(
        tokens!(PricingItemReferenceState),
        ["unreserved", "confirmation_pending", "confirmed", "lost"]
    );
    assert_eq!(
        tokens!(PricingRevisionState),
        ["draft", "pending", "scheduled", "published", "superseded"]
    );
    assert_eq!(
        tokens!(PricingResolvedRevisionState),
        ["published", "superseded", "scheduled"]
    );
    assert_eq!(
        tokens!(PricingReferenceOpKind),
        ["create", "delete", "rereserve", "attach", "release"]
    );
    assert_eq!(
        tokens!(PricingReferenceOpState),
        ["reserving", "written", "cancelling", "releasing", "done"]
    );
    assert_eq!(
        tokens!(PricingReferenceOpRefKind),
        ["price_book_entry", "plan_item"]
    );
    assert_eq!(
        tokens!(PricingUnitState),
        ["pending", "approved", "rejected", "withdrawn"]
    );
    assert_eq!(tokens!(PricingApprovalKind), ["prices", "plan_revision"]);
    assert_eq!(tokens!(PricingDecisionKind), ["approve", "reject"]);
    assert_eq!(
        tokens!(PricingVoteOutcome),
        ["pending", "applied", "rejected", "withdrawn"]
    );
    assert_eq!(tokens!(PricingBillingTiming), ["advance", "arrears"]);
    assert_eq!(tokens!(PricingResolveSource), ["entry", "sku", "tenant"]);
}

/// D-522: a reference op's reason is `book_archived`, the token a release op's work stores.
#[test]
fn a_reference_op_reason_is_book_archived() {
    assert_eq!(
        round_trips(
            PricingReferenceOpReason::ALL,
            PricingReferenceOpReason::as_str,
            PricingReferenceOpReason::stored,
        ),
        [crate::infra::reference_work::BOOK_ARCHIVED_REASON]
    );
}

/// D-520 (amended 2026-10-04): a pinned price's status is `cancelled` alone, the entry list's token
/// for a cancelled price.
#[test]
fn a_pinned_price_status_is_cancelled_alone() {
    assert_eq!(
        round_trips(
            PricingPinnedPriceStatus::ALL,
            PricingPinnedPriceStatus::as_str,
            PricingPinnedPriceStatus::stored,
        ),
        [PricingPriceStatus::Cancelled.as_str()]
    );
}

#[test]
fn a_sku_entry_status_carries_priced_scheduled_and_unpriced() {
    assert_eq!(
        round_trips(
            PricingSkuEntryStatus::ALL,
            PricingSkuEntryStatus::as_str,
            PricingSkuEntryStatus::stored,
        ),
        ["priced", "scheduled", "unpriced"]
    );
}

/// The domain's value `d` and the wire value it maps to carry one token.
fn same<D: Copy, W: From<D>>(
    domain: &[D],
    wire: &[W],
    d: fn(D) -> &'static str,
    w: fn(W) -> &'static str,
) {
    assert_eq!(
        domain.len(),
        wire.len(),
        "{:?}",
        domain.iter().map(|&v| d(v)).collect::<Vec<_>>()
    );
    for &v in domain {
        assert_eq!(w(W::from(v)), d(v));
    }
}

/// A set with a domain enum carries the domain's own token for every value.
#[test]
fn a_mapped_set_carries_the_domain_token() {
    use crate::domain::reference_op::RefKind;
    same(
        ChargeKind::ALL,
        PricingChargeKind::ALL,
        ChargeKind::as_str,
        PricingChargeKind::as_str,
    );
    same(
        Model::ALL,
        PricingModel::ALL,
        Model::as_str,
        PricingModel::as_str,
    );
    same(
        EntryReference::ALL,
        PricingEntryReferenceState::ALL,
        EntryReference::as_str,
        PricingEntryReferenceState::as_str,
    );
    same(
        Eligibility::ALL,
        PricingEligibility::ALL,
        Eligibility::as_str,
        PricingEligibility::as_str,
    );
    same(
        PriceState::ALL,
        PricingPriceState::ALL,
        PriceState::as_str,
        PricingPriceState::as_str,
    );
    same(
        DisplayStatus::ALL,
        PricingPriceStatus::ALL,
        DisplayStatus::as_str,
        PricingPriceStatus::as_str,
    );
    same(
        ItemReference::ALL,
        PricingItemReferenceState::ALL,
        ItemReference::as_str,
        PricingItemReferenceState::as_str,
    );
    same(
        RevisionState::ALL,
        PricingRevisionState::ALL,
        RevisionState::as_str,
        PricingRevisionState::as_str,
    );
    same(
        OpKind::ALL,
        PricingReferenceOpKind::ALL,
        OpKind::as_str,
        PricingReferenceOpKind::as_str,
    );
    same(
        OpState::ALL,
        PricingReferenceOpState::ALL,
        OpState::as_str,
        PricingReferenceOpState::as_str,
    );
    same(
        Source::ALL,
        PricingResolveSource::ALL,
        Source::as_str,
        PricingResolveSource::as_str,
    );
    same(
        &[
            UnitState::Pending,
            UnitState::Approved,
            UnitState::Rejected,
            UnitState::Withdrawn,
        ],
        PricingUnitState::ALL,
        UnitState::as_str,
        PricingUnitState::as_str,
    );
    same(
        &crate::infra::approval_kinds::Kind::ALL,
        PricingApprovalKind::ALL,
        crate::infra::approval_kinds::Kind::as_str,
        PricingApprovalKind::as_str,
    );
    same(
        &[Verdict::Approve, Verdict::Reject],
        PricingDecisionKind::ALL,
        Verdict::as_str,
        PricingDecisionKind::as_str,
    );
    // Read from its stored token (no `From`): every domain token reads back, and no other.
    assert_eq!(PricingReferenceOpRefKind::ALL.len(), RefKind::ALL.len());
    for &d in RefKind::ALL {
        let wire = PricingReferenceOpRefKind::stored(d.as_str(), &"op").unwrap();
        assert_eq!(wire.as_str(), d.as_str());
    }
}

/// A stored token outside its set is `CorruptRow` naming the row and the token: never a panic,
/// never a value the schema does not hold. The period and the resolved state read no domain enum,
/// so an empty token and a draft revision are outside too.
#[test]
fn a_stored_token_outside_its_set_is_a_corrupt_row_naming_it() {
    let id = uuid::Uuid::nil();
    for (error, set) in [
        (
            PricingChargeKind::stored("weekly", &format_args!("entry {id} charge_kind"))
                .unwrap_err(),
            "PricingChargeKind",
        ),
        (
            PricingPeriod::stored("", &format_args!("entry {id} period")).unwrap_err(),
            "PricingPeriod",
        ),
        (
            PricingResolvedRevisionState::stored("draft", &format_args!("revision {id} state"))
                .unwrap_err(),
            "PricingResolvedRevisionState",
        ),
        (
            PricingBillingTiming::stored("ADVANCE", &format_args!("settings {id} default_timing"))
                .unwrap_err(),
            "PricingBillingTiming",
        ),
    ] {
        let RepoError::CorruptRow(detail) = error else {
            panic!("{error:?}");
        };
        assert!(detail.contains(&id.to_string()), "{detail}");
        assert!(detail.ends_with(&format!("is not a {set}")), "{detail}");
    }
}
