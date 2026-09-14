//! [`declared_act`]'s four worlds, over hand-built compositions.
//!
//! The classification is a pure function of two [`CompositionDraft`]s, so it is
//! tested here exhaustively and cheaply; `tests/rest_bundles.rs` drives the two
//! that matter through the real router, which is what proves the operands are the
//! ones the route can actually resolve. Both layers are needed and neither
//! substitutes: a route probe cannot enumerate the arms without staging four plan
//! revisions, and these cases cannot tell whether anything reads them.

use uuid::Uuid;

use super::{composition_change_set, declared_act, rev_share_change_set};
use crate::domain::bundle::{Absorber, Party, PartyShare, RevShareGroup};
use crate::domain::materiality::triggers::Trigger;
use crate::infra::storage::repo::{BundleComponentDraft, CompositionDraft};

/// A component on a fixed plan, so two drafts naming the same plan compare equal.
fn component(plan: u128) -> BundleComponentDraft {
    BundleComponentDraft {
        component_plan_id: Uuid::from_u128(plan),
        included_sku_id: Uuid::from_u128(plan + 1),
        min_qty: None,
        max_qty: None,
    }
}

/// One vendor's group, split between two named parties.
fn group(vendor: u128, first_bp: i32, second_bp: i32) -> RevShareGroup {
    RevShareGroup {
        vendor_sku_id: Uuid::from_u128(vendor),
        platform_cut_bp: 0,
        residual_absorber: Absorber::Platform,
        parties: vec![
            PartyShare {
                party: Party::new("alpha").expect("a named party"),
                share_bp: first_bp,
            },
            PartyShare {
                party: Party::new("beta").expect("a named party"),
                share_bp: second_bp,
            },
        ],
    }
}

fn draft(components: Vec<BundleComponentDraft>, groups: Vec<RevShareGroup>) -> CompositionDraft {
    CompositionDraft {
        components,
        rev_share_groups: groups,
    }
}

/// The act a change set declares, as the trigger registry names it.
fn act_of(published: Option<&CompositionDraft>, candidate: &CompositionDraft) -> Option<Trigger> {
    declared_act(published, candidate).act()
}

/// **D-104's first clause, "bundle creation".** No revision of this plan has ever
/// been live, so there is nothing the composition could be a re-split *of*.
#[test]
fn a_composition_with_no_live_predecessor_is_the_composition_act() {
    let candidate = draft(vec![component(1)], vec![group(9, 5000, 5000)]);

    assert_eq!(
        act_of(None, &candidate),
        Some(Trigger::BundleComposition),
        "a first composition is D-104's `bundle creation`, whatever its rev-share holds"
    );
}

/// **The claim the whole change exists to make.** The component set is untouched
/// and the split moved, so the record says what a vendor is paid moved and what the
/// customer receives did not.
#[test]
fn an_unchanged_component_set_with_a_moved_split_is_the_rev_share_act() {
    let published = draft(vec![component(1)], vec![group(9, 5000, 5000)]);
    let candidate = draft(vec![component(1)], vec![group(9, 7000, 3000)]);

    assert_eq!(
        act_of(Some(&published), &candidate),
        Some(Trigger::RevenueShareChange),
        "D-104 registers two triggers so this case is expressible; it is the only \
         one that answers the narrower of the two"
    );
}

/// The **positive control** for the case above, and the reason the precedence runs
/// the way it does: a swap that also re-splits is a swap. Reporting it as a
/// re-split would assert that the customer's side did not move, which is the one
/// mis-naming of the two that misleads.
#[test]
fn a_component_change_is_the_composition_act_even_when_the_split_moved_too() {
    let published = draft(vec![component(1)], vec![group(9, 5000, 5000)]);
    let candidate = draft(vec![component(2)], vec![group(9, 7000, 3000)]);

    assert_eq!(
        act_of(Some(&published), &candidate),
        Some(Trigger::BundleComposition),
        "a component swap is a composition change however its shares moved"
    );
}

/// A component change on its own, with the split held still — the other half of
/// the control, so the case above cannot pass by ignoring the components.
#[test]
fn a_component_change_alone_is_the_composition_act() {
    let published = draft(vec![component(1)], vec![group(9, 5000, 5000)]);
    let candidate = draft(vec![component(1), component(2)], vec![group(9, 5000, 5000)]);

    assert_eq!(
        act_of(Some(&published), &candidate),
        Some(Trigger::BundleComposition),
        "adding a component is D-104's `component add/remove/replace`"
    );
}

/// A re-publish of an unchanged composition. It is a legal act — a revision may
/// publish more than once — and it is still material; what it is not is a
/// re-split, so it must not claim the narrower token.
#[test]
fn an_unchanged_composition_is_the_composition_act_and_not_the_rev_share_one() {
    let composition = draft(vec![component(1)], vec![group(9, 5000, 5000)]);

    assert_eq!(
        act_of(Some(&composition), &composition),
        Some(Trigger::BundleComposition),
        "nothing moved, so nothing may be reported as a vendor-payout change"
    );
}

/// A group **added** where there was none is a rev-share change, not a component
/// one: the vendor set a bundle pays is its own axis.
#[test]
fn a_group_added_to_an_unchanged_component_set_is_the_rev_share_act() {
    let published = draft(vec![component(1)], Vec::new());
    let candidate = draft(vec![component(1)], vec![group(9, 5000, 5000)]);

    assert_eq!(
        act_of(Some(&published), &candidate),
        Some(Trigger::RevenueShareChange),
        "a group that did not exist before is a change to who is paid"
    );
}

/// The absorber is one of D-104's three named rev-share fields, and it is the one a
/// comparison on shares alone would miss — every party's `share_bp` is untouched
/// here and what moved is who takes the residual.
#[test]
fn a_moved_residual_absorber_is_the_rev_share_act() {
    let published = draft(vec![component(1)], vec![group(9, 5000, 5000)]);
    let mut moved = group(9, 5000, 5000);
    moved.residual_absorber = Absorber::Party(Party::new("alpha").expect("a named party"));
    let candidate = draft(vec![component(1)], vec![moved]);

    assert_eq!(
        act_of(Some(&published), &candidate),
        Some(Trigger::RevenueShareChange),
        "D-104 names `residual_absorber_party` beside the two share fields"
    );
}

/// `platform_cut_bp`, the third of D-104's three, for the same reason.
#[test]
fn a_moved_platform_cut_is_the_rev_share_act() {
    let published = draft(vec![component(1)], vec![group(9, 5000, 5000)]);
    let mut moved = group(9, 4000, 5000);
    moved.platform_cut_bp = 1000;
    let candidate = draft(vec![component(1)], vec![moved]);

    assert_eq!(
        act_of(Some(&published), &candidate),
        Some(Trigger::RevenueShareChange),
        "the platform's own cut is a rev-share field"
    );
}

/// **What this diff cannot see, pinned where a reader meets it.**
///
/// D-104 puts `price_basis` and `invoiceItemization` under the rev-share trigger
/// and [`CompositionDraft`] carries neither, because `pricing_bundle` is keyed by
/// `bundle_id` alone and holds them un-revisioned — **D-206**, which owns that
/// consequence and has already decided the fix. This case is not a claim that the
/// omission is harmless: it is the instrument that reddens the day D-206's
/// revision-scoped carrier lands and the two fields become comparable, so the
/// author of that migration is told this classifier has to grow rather than
/// discovering it from a stale doc.
///
/// It is safe today for the reason D-206 measured rather than assumed: *"the only
/// writer of either column is `BundleRepo::create`, and no mounted route mutates
/// them"*. A field that cannot move cannot be missed by a diff.
#[test]
fn the_declared_act_reads_only_what_a_revision_can_carry() {
    // Two fields and no more. Destructured exhaustively rather than counted, so a
    // third member of the composition fails to **compile** here instead of being
    // silently excluded from the classification — the axis-enumeration failure this
    // repository has paid for twice, once on a scope key and once on a row tag.
    let CompositionDraft {
        components,
        rev_share_groups,
    } = draft(vec![component(1)], vec![group(9, 5000, 5000)]);

    assert_eq!(components.len(), 1);
    assert_eq!(rev_share_groups.len(), 1);
}

/// The two constructors are the only answers, and they are distinct — a
/// classification that returned one of them for every input would pass every case
/// above that expects it and nothing would say so.
#[test]
fn the_two_declarations_are_distinct_acts() {
    assert_ne!(
        composition_change_set().act(),
        rev_share_change_set().act(),
        "D-104's two triggers must not collapse to one token"
    );
    assert_eq!(
        composition_change_set().act(),
        Some(Trigger::BundleComposition)
    );
    assert_eq!(
        rev_share_change_set().act(),
        Some(Trigger::RevenueShareChange)
    );
}
