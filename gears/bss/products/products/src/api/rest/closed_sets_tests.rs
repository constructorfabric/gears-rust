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
fn each_set_carries_its_stored_tokens_on_the_wire() {
    macro_rules! tokens {
        ($t:ident) => {
            round_trips($t::ALL, $t::as_str, $t::stored)
        };
    }
    assert_eq!(
        tokens!(ProductsSkuType),
        ["recurring", "usage", "one_time", "bundle"]
    );
    assert_eq!(
        tokens!(ProductsLifecycle),
        ["draft", "published", "deprecated", "retired"]
    );
    assert_eq!(tokens!(ProductsBillingTiming), ["advance", "arrears"]);
    assert_eq!(tokens!(ProductsCategoryStatus), ["active", "retired"]);
    assert_eq!(
        tokens!(ProductsUnitState),
        ["pending", "approved", "rejected", "withdrawn"]
    );
    assert_eq!(
        tokens!(ProductsApprovalKind),
        ["sku_publish", "sku_change", "sku_retire"]
    );
    assert_eq!(tokens!(ProductsDecisionKind), ["approve", "reject"]);
    assert_eq!(
        tokens!(ProductsVoteOutcome),
        ["pending", "applied", "rejected", "withdrawn"]
    );
    assert_eq!(
        tokens!(ProductsReferenceKind),
        ["price_book_entry", "plan_item", "sold_as"]
    );
    assert_eq!(
        tokens!(ProductsReferenceState),
        ["reserved", "confirmed", "released"]
    );
}

/// The source's value and the wire value it maps to carry one token: the SDK enums serialize in
/// `snake_case`, the approval enums answer `as_str`.
fn same<S: Copy + serde::Serialize, W: From<S>>(
    source: &[S],
    wire: &[W],
    w: fn(W) -> &'static str,
) {
    assert_eq!(source.len(), wire.len());
    for &s in source {
        assert_eq!(serde_json::to_value(s).unwrap(), w(W::from(s)));
    }
}

/// A set with an SDK or approval enum carries that enum's own token for every value.
#[test]
fn a_mapped_set_carries_the_source_token() {
    same(
        &[
            SkuType::Recurring,
            SkuType::Usage,
            SkuType::OneTime,
            SkuType::Bundle,
        ],
        ProductsSkuType::ALL,
        ProductsSkuType::as_str,
    );
    same(
        &[
            Lifecycle::Draft,
            Lifecycle::Published,
            Lifecycle::Deprecated,
            Lifecycle::Retired,
        ],
        ProductsLifecycle::ALL,
        ProductsLifecycle::as_str,
    );
    same(
        &[BillingTiming::Advance, BillingTiming::Arrears],
        ProductsBillingTiming::ALL,
        ProductsBillingTiming::as_str,
    );
    same(
        &[
            ReferenceKind::PriceBookEntry,
            ReferenceKind::PlanItem,
            ReferenceKind::SoldAs,
        ],
        ProductsReferenceKind::ALL,
        ProductsReferenceKind::as_str,
    );
    same(
        &[
            ReferenceState::Reserved,
            ReferenceState::Confirmed,
            ReferenceState::Released,
        ],
        ProductsReferenceState::ALL,
        ProductsReferenceState::as_str,
    );
    same(
        &[
            UnitState::Pending,
            UnitState::Approved,
            UnitState::Rejected,
            UnitState::Withdrawn,
        ],
        ProductsUnitState::ALL,
        ProductsUnitState::as_str,
    );
    same(
        &[Verdict::Approve, Verdict::Reject],
        ProductsDecisionKind::ALL,
        ProductsDecisionKind::as_str,
    );
}

/// The approval kind carries the domain's own token for every kind (P-D-227).
#[test]
fn the_approval_kind_carries_the_domain_token() {
    assert_eq!(ApprovalKind::ALL.len(), ProductsApprovalKind::ALL.len());
    for kind in ApprovalKind::ALL {
        assert_eq!(ProductsApprovalKind::from(kind).as_str(), kind.as_str());
        assert_eq!(ApprovalKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(ApprovalKind::parse("promotion"), None);
}

/// A stored token outside its set is `CorruptRow` naming the row and the token: never a panic,
/// never a value the schema does not hold.
#[test]
fn a_stored_token_outside_its_set_is_a_corrupt_row_naming_it() {
    let id = uuid::Uuid::nil();
    for (error, set) in [
        (
            ProductsCategoryStatus::stored("archived", &format_args!("category {id} status"))
                .unwrap_err(),
            "ProductsCategoryStatus",
        ),
        (
            ProductsReferenceKind::stored("offer", &format_args!("reference {id} ref_kind"))
                .unwrap_err(),
            "ProductsReferenceKind",
        ),
        (
            ProductsReferenceState::stored("", &format_args!("reference {id} state")).unwrap_err(),
            "ProductsReferenceState",
        ),
    ] {
        let RepoError::CorruptRow(detail) = error else {
            panic!("{error:?}");
        };
        assert!(detail.contains(&id.to_string()), "{detail}");
        assert!(detail.ends_with(&format!("is not a {set}")), "{detail}");
    }
}
