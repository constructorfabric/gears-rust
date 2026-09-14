//! Tests for `pricingSnapshotRef` and its two-state version ref.
//!
//! **These guard the domain statement, not the live pin**, and the distinction
//! is load-bearing enough that reading it off `snapshot.rs`'s module doc is not
//! good enough. `PricingSnapshotRef`'s only producer is
//! `PublishReceipt::snapshot_ref`, which nothing outside `domain::publish_tests`
//! calls, and `PricingSnapshotRef::finalize` has no caller outside this file. The
//! mechanism that actually protects a posted period's snapshot is a row
//! compare-and-swap on `pricing_catalog_version_ref`
//! (`catalog_version_ref_repo::finalize`) — so the immutability cases below would
//! stay green through any regression of it. What they hold is that the *type*
//! cannot be re-pointed, which is what makes it safe to give the composition one
//! home the day a second emitter needs one.

use bss_pricing_sdk::CatalogVersion;
use uuid::Uuid;

use super::{PricingSnapshotRef, VersionRef};
use crate::domain::error::DomainError;

fn stamped() -> PricingSnapshotRef {
    PricingSnapshotRef::new(
        VersionRef::Pending("pending-42".to_owned()),
        vec![Uuid::from_u128(7), Uuid::from_u128(8)],
        "policy-v3".to_owned(),
    )
}

#[test]
fn a_publish_stamps_a_pending_ref() {
    // Between the publish commit and CatalogVersionPublished the ref has a
    // real identity — the registry's handle — which is why this is not an
    // absent version.
    let snapshot = stamped();

    assert!(!snapshot.version_ref().is_committed());
    assert_eq!(snapshot.version_ref().pending_ref(), Some("pending-42"));
    assert_eq!(snapshot.version_ref().committed(), None);
}

#[test]
fn finalize_moves_pending_to_committed() {
    let snapshot = stamped()
        .finalize(CatalogVersion::new(12))
        .expect("a pending ref finalizes");

    assert!(snapshot.version_ref().is_committed());
    assert_eq!(
        snapshot.version_ref().committed(),
        Some(CatalogVersion::new(12))
    );
    assert_eq!(snapshot.version_ref().pending_ref(), None);
}

#[test]
fn a_committed_ref_refuses_to_be_re_finalized() {
    // A duplicate CatalogVersionPublished carrying a different version would
    // otherwise silently re-point a pin that posted periods already resolved
    // through — and a posted period never re-queries mutable catalog rows, so
    // nothing downstream would ever notice.
    let committed = stamped()
        .finalize(CatalogVersion::new(12))
        .expect("first finalize");

    let err = committed
        .finalize(CatalogVersion::new(13))
        .expect_err("a committed ref is immutable");

    assert!(matches!(err, DomainError::LifecycleForbidden(_)));
}

#[test]
fn re_finalizing_to_the_same_version_is_still_refused() {
    // Not treated as an idempotent no-op: "same version" is a fact only the
    // caller can assert, and accepting it would make the guard depend on the
    // argument rather than on the state.
    let committed = VersionRef::Pending("pending-42".to_owned())
        .finalize(CatalogVersion::new(12))
        .expect("first finalize");

    assert!(committed.finalize(CatalogVersion::new(12)).is_err());
}

// The composition is asserted where something happens to it, and nowhere else:
// `finalizing_changes_nothing_but_the_version_ref` below carries both non-version
// parts across the one transition the type has. A case that built the value and
// read the same three fields back through the accessors would exercise no logic
// at all, and could not produce the failure it would be named for — a part
// dropped between two views.

#[test]
fn finalizing_changes_nothing_but_the_version_ref() {
    // The resolved ids and the policy version are frozen at publish; the
    // registry's later commit assigns addressability, not content.
    let before = stamped();
    let after = before
        .clone()
        .finalize(CatalogVersion::new(12))
        .expect("finalize");

    assert_eq!(after.price_ids(), before.price_ids());
    assert_eq!(
        after.evaluation_policy_version(),
        before.evaluation_policy_version()
    );
}
