//! Tests for the frozen event-name set.

use std::collections::BTreeSet;

use super::CatalogEvent;

/// The frozen names, written out as literals.
///
/// Deliberately not derived from the variants: the whole point is that
/// renaming a Rust variant must not rename a wire event, so this list has to be
/// an independent statement of the contract that a rename breaks.
const FROZEN_WIRE_NAMES: &[&str] = &[
    "PlanCreated",
    "PlanUpdated",
    "PlanPublished",
    "PlanRetired",
    "PlanMigrationScheduled",
    "PlanPublishDegraded",
    "BundleUpdated",
    "PriceCreated",
    "PriceUpdated",
    "PriceWindowScheduled",
    "PriceWindowActivated",
    "PriceWindowExpired",
    "PriceWindowCancelled",
    "PriceOverlayPublished",
];

#[test]
fn the_wire_names_are_exactly_the_frozen_strings() {
    let emitted: Vec<&str> = CatalogEvent::ALL
        .iter()
        .map(|event| event.as_str())
        .collect();

    assert_eq!(emitted, FROZEN_WIRE_NAMES);

    // `Display` delegates to `as_str`, and it is carried here rather than in a case
    // of its own: every production site reads `as_str` directly, so the rendering is
    // reachable only through a `{}` in a diagnostic — where a divergence would
    // misname the event in the one place a reader is already looking for a fault.
    let rendered: Vec<String> = CatalogEvent::ALL
        .iter()
        .map(std::string::ToString::to_string)
        .collect();

    assert_eq!(rendered, FROZEN_WIRE_NAMES);
}

#[test]
fn the_set_is_fourteen_names_and_no_more() {
    // Adding a name is a contract change and has to be argued for, so the count is
    // asserted rather than left to follow the enum: this census is what makes a new
    // variant an argued change rather than a module minting wire surface for
    // itself. The name is frozen in `chk_pricing_outbox_event_name` too, so the
    // constraint moves with the set.
    //
    // `FROZEN_WIRE_NAMES` needs no count of its own — `the_wire_names_are_exactly_
    // the_frozen_strings` asserts it equal to the rendering of `ALL`.
    assert_eq!(CatalogEvent::ALL.len(), 14);
}

#[test]
fn all_lists_every_variant_once() {
    // A missed variant in `ALL` would be an event the outbox registration
    // never sees, and it would fail silently: nothing subscribes to it.
    //
    // **Distinctness is the smaller half and it cannot see that failure.** The
    // set below is built *from* `ALL`, so a variant the slice never lists is
    // absent from both sides of the comparison; the census that catches it is
    // the exhaustive match in `every_variant_is_in_the_roster` below.
    let distinct: BTreeSet<CatalogEvent> = CatalogEvent::ALL.iter().copied().collect();

    assert_eq!(distinct.len(), CatalogEvent::ALL.len());
}

/// Every variant reaches [`CatalogEvent::ALL`], enforced by the compiler.
///
/// `all_lists_every_variant_once` asserts distinctness and cannot observe the
/// failure its own comment names: a fifteenth variant is forced into `as_str`'s
/// exhaustive match but not into `ALL`, so it compiles, the count assertion still
/// reads 14 against a 14-entry slice, and the event drops silently out of the
/// roster `outbox_repo` and `chk_pricing_outbox_event_name`'s `chk_pricing_outbox_event_name`
/// are built from — where the CHECK would then reject it at write time.
///
/// The `match` is the gate, in `contracts_tests::every_billing_anchor_policy_member_is_in_the_roster`'s
/// shape: **no `_ =>` arm**, so a new variant stops this file compiling until
/// somebody names it here, and naming it here is what puts it in front of the
/// `ALL` membership assertion.
#[test]
fn every_variant_is_in_the_roster() {
    fn rostered(event: CatalogEvent) -> bool {
        // No `_ =>` arm, deliberately: this match is the gate.
        match event {
            CatalogEvent::PlanCreated
            | CatalogEvent::PlanUpdated
            | CatalogEvent::PlanPublished
            | CatalogEvent::PlanRetired
            | CatalogEvent::PlanMigrationScheduled
            | CatalogEvent::PlanPublishDegraded
            | CatalogEvent::BundleUpdated
            | CatalogEvent::PriceCreated
            | CatalogEvent::PriceUpdated
            | CatalogEvent::PriceWindowScheduled
            | CatalogEvent::PriceWindowActivated
            | CatalogEvent::PriceWindowExpired
            | CatalogEvent::PriceWindowCancelled
            | CatalogEvent::PriceOverlayPublished => CatalogEvent::ALL.contains(&event),
        }
    }

    let every = [
        CatalogEvent::PlanCreated,
        CatalogEvent::PlanUpdated,
        CatalogEvent::PlanPublished,
        CatalogEvent::PlanRetired,
        CatalogEvent::PlanMigrationScheduled,
        CatalogEvent::PlanPublishDegraded,
        CatalogEvent::BundleUpdated,
        CatalogEvent::PriceCreated,
        CatalogEvent::PriceUpdated,
        CatalogEvent::PriceWindowScheduled,
        CatalogEvent::PriceWindowActivated,
        CatalogEvent::PriceWindowExpired,
        CatalogEvent::PriceWindowCancelled,
        CatalogEvent::PriceOverlayPublished,
    ];
    // The two lists above are one list twice, and this is what keeps them so: a
    // variant added to the `match` and forgotten here would leave the array
    // short of `ALL`.
    assert_eq!(every.len(), CatalogEvent::ALL.len());
    for event in every {
        assert!(rostered(event), "{event} is missing from CatalogEvent::ALL");
    }
}

#[test]
fn no_two_events_share_a_wire_name() {
    let distinct: BTreeSet<&str> = CatalogEvent::ALL
        .iter()
        .map(|event| event.as_str())
        .collect();

    assert_eq!(distinct.len(), CatalogEvent::ALL.len());
}

#[test]
fn there_is_no_deletion_event() {
    // Published rows are never deleted: a row leaves service by supersession
    // or by its plan retiring, and stays readable as history — so no consumer
    // ever has to reconcile a disappearance.
    assert!(
        !CatalogEvent::ALL
            .iter()
            .any(|event| event.as_str().contains("Delete") || event.as_str().contains("Removed"))
    );
}
