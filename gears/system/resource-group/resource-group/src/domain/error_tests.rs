// Created: 2026-07-30 by Constructor Tech
use super::*;

use toolkit_odata::Error as OdataError;

/// ML-7391: `From<toolkit_odata::Error> for DomainError` is the single
/// place that decides whether a `paginate_odata` failure is the caller's
/// fault (400) or ours (500) -- see the contract comment on `toolkit_odata`'s
/// `Error` enum (`libs/toolkit-odata/src/lib.rs`) and on this `impl` above.
/// Before this test module it had no direct unit test at all: it was only
/// exercised indirectly, through `MembershipRepository::list_memberships`'s
/// REST tests. `GroupRepository::list_groups` and
/// `TypeRepository::list_types` instead folded every `paginate_odata`
/// failure into a blanket `DomainError::database(..)`, silently bypassing
/// this classifier -- these tests pin the classifier itself so that defect
/// class cannot recur unnoticed on any of its three call sites.
///
/// All 13 client-caused variants (bad `$filter`, unknown `$orderby` field,
/// malformed/stale/mismatched cursor, bad `$top`) are covered individually
/// rather than sampled, so moving one of them into the `Database` arm by
/// mistake turns this test red instead of silently changing the wire
/// contract. The classifier itself has no catch-all: both arms are spelled
/// out, so a variant added upstream fails the build in `error.rs` — this
/// list then needs the same update.
#[test]
fn client_variants_classify_as_validation() {
    let client_variants: Vec<OdataError> = vec![
        OdataError::InvalidFilter("bad filter".to_owned()),
        OdataError::InvalidOrderByField("not_a_field".to_owned()),
        OdataError::OrderMismatch,
        OdataError::FilterMismatch,
        OdataError::InvalidCursor,
        OdataError::InvalidLimit,
        OdataError::OrderWithCursor,
        OdataError::CursorInvalidBase64,
        OdataError::CursorInvalidJson,
        OdataError::CursorInvalidVersion,
        OdataError::CursorInvalidKeys,
        OdataError::CursorInvalidFields,
        OdataError::CursorInvalidDirection,
    ];

    for variant in client_variants {
        let label = format!("{variant:?}");
        let mapped = DomainError::from(variant);
        assert!(
            matches!(mapped, DomainError::Validation { .. }),
            "{label} must classify as DomainError::Validation (400), got {mapped:?} instead"
        );
    }
}

/// A genuine backend failure (`toolkit_odata::Error::Db`, surfaced by
/// `paginate_odata` when the underlying query itself fails) must stay
/// `DomainError::Database` (500) -- it is never the caller's fault.
#[test]
fn db_variant_classifies_as_database() {
    let mapped = DomainError::from(OdataError::Db("connection reset by peer".to_owned()));
    assert!(
        matches!(mapped, DomainError::Database(_)),
        "Error::Db must classify as DomainError::Database (500), got {mapped:?} instead"
    );
}

/// `Error::ParsingUnavailable` signals a configuration/infra problem (the
/// `OData` parser feature is unavailable at runtime), not a caller mistake
/// -- must also stay `DomainError::Database` (500).
#[test]
fn parsing_unavailable_variant_classifies_as_database() {
    let mapped = DomainError::from(OdataError::ParsingUnavailable("no parser configured"));
    assert!(
        matches!(mapped, DomainError::Database(_)),
        "Error::ParsingUnavailable must classify as DomainError::Database (500), got {mapped:?} instead"
    );
}
