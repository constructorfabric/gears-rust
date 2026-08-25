use super::*;

// ---------------------------------------------------------------------------
// The typed values.
// ---------------------------------------------------------------------------

#[test]
fn a_basis_round_trips_through_its_stored_token() {
    for basis in PriceBasis::ALL {
        assert_eq!(PriceBasis::parse(basis.as_str()), Some(*basis));
    }
}

#[test]
fn a_third_basis_is_not_a_basis() {
    assert_eq!(PriceBasis::parse("sum_of_the_parts"), None);
}

#[test]
fn an_itemization_round_trips_through_its_stored_token() {
    for layout in InvoiceItemization::ALL {
        assert_eq!(InvoiceItemization::parse(layout.as_str()), Some(*layout));
    }
}

#[test]
fn a_party_may_not_spell_the_platform_sentinel() {
    assert_eq!(Party::new("platform"), None);
}

#[test]
fn a_party_may_not_be_blank() {
    assert_eq!(Party::new("   "), None);
}

// ---------------------------------------------------------------------------
// `inst-rs-residual` / D-07 — the tolerance and the normalization.
// ---------------------------------------------------------------------------

/// One group, spelled out so each case varies exactly one thing.
fn group(platform_cut_bp: i32, absorber: Absorber, shares: &[(&str, i32)]) -> RevShareGroup {
    RevShareGroup {
        vendor_sku_id: uuid::Uuid::from_u128(0x_5e_11),
        platform_cut_bp,
        residual_absorber: absorber,
        parties: shares
            .iter()
            .map(|(name, bp)| PartyShare {
                party: Party::new(name).expect("a well-formed party"),
                share_bp: *bp,
            })
            .collect(),
    }
}

/// An exact split needs no adjustment, and says so.
#[test]
fn an_exact_split_normalizes_to_itself_with_no_adjustment() {
    let reconciled = reconcile(&group(
        1000,
        Absorber::Platform,
        &[("vendor-a", 4500), ("vendor-b", 4500)],
    ))
    .expect("an exact split reconciles");

    assert_eq!(reconciled.adjustment_bp, 0);
    assert_eq!(
        reconciled.effective_shares,
        vec![
            (Party::new("vendor-a").unwrap(), 4500),
            (Party::new("vendor-b").unwrap(), 4500)
        ]
    );
}

/// The PRD's own worked example, and the reason D-07 exists: three parties at
/// 33.33% sum to 9999 bp, one short.
///
/// With the platform absorbing, no party's effective share moves and the
/// **platform cut** takes the basis point — which is what "the absorber's
/// effective share is adjusted" means when the absorber is the platform.
#[test]
fn the_thirty_three_by_three_split_normalizes_onto_the_platform() {
    // Cut of zero, three parties at 33.33%: 9999 bp authored, one short.
    let reconciled = reconcile(&group(
        0,
        Absorber::Platform,
        &[("a", 3333), ("b", 3333), ("c", 3333)],
    ))
    .expect("a 1 bp residual is within tolerance");

    assert_eq!(
        reconciled.adjustment_bp, 1,
        "the residual is one basis point"
    );
    assert_eq!(
        reconciled.effective_platform_cut_bp, 1,
        "the platform's cut is where the platform absorbs"
    );
    assert!(
        reconciled
            .effective_shares
            .iter()
            .all(|(_, bp)| *bp == 3333),
        "no party's effective share moves when the platform absorbs"
    );
    assert_eq!(reconciled.sums_to(), 10_000);
}

/// A nominated party absorbs instead, and it is that party's effective share
/// that moves — the typed value is retained beside it for audit.
#[test]
fn a_nominated_party_absorbs_the_residual_on_its_own_effective_share() {
    let reconciled = reconcile(&group(
        0,
        Absorber::Party(Party::new("a").expect("party")),
        &[("a", 3333), ("b", 3333), ("c", 3333)],
    ))
    .expect("a 1 bp residual is within tolerance");

    assert_eq!(
        reconciled.effective_platform_cut_bp, 0,
        "the cut is untouched when a party absorbs"
    );
    assert_eq!(
        reconciled.effective_shares,
        vec![
            (Party::new("a").unwrap(), 3334),
            (Party::new("b").unwrap(), 3333),
            (Party::new("c").unwrap(), 3333),
        ]
    );
    assert_eq!(reconciled.sums_to(), 10_000);
}

/// The residual has a sign: an over-authored split is normalized downward.
#[test]
fn an_over_authored_split_is_normalized_downward() {
    let reconciled = reconcile(&group(
        1000,
        Absorber::Party(Party::new("a").expect("party")),
        &[("a", 4501), ("b", 4500)],
    ))
    .expect("a 1 bp overshoot is within tolerance");

    assert_eq!(reconciled.adjustment_bp, -1);
    assert_eq!(reconciled.effective_shares[0].1, 4500);
    assert_eq!(reconciled.sums_to(), 10_000);
}

/// **The authored total is formed without wrapping** (Z5-12).
///
/// `reconcile` is a domain function over a `RevShareGroup` whose party list has no
/// length bound, so the `+` and the `sum` that form `authored` — and the `-` that
/// turns it into a residual — were three unchecked accumulations over
/// caller-supplied values. A wrapped `authored` produces a residual far outside the
/// tolerance, so the refusal path is entered rather than a wrong split published,
/// but it is entered carrying a number that is not the sum of anything, and a debug
/// build panics before it gets there.
///
/// **What this case does not prove is reachability, and the doc used to claim it
/// did**: it said the group "reaches it from the wire before persistence". At HEAD
/// both callers of `reconcile` read stored rows, every operand is CHECK-bounded to
/// `0..=10000` on both engines, and `api::rest::bundles::draft_of`'s unbounded
/// `platform_cut_bp` is refused by the store before it could get here. So these are
/// defence-in-depth assertions on a domain function, which is what a domain
/// function's own arithmetic discipline is worth — see `reconcile`'s comment.
///
/// A structural refusal rather than a residual one, because a total that does not
/// exist is not a total that is 4 bp out: `RESIDUAL_OVER_TOLERANCE` tells an
/// operator how far from the whole they are, and that number would be a fiction.
#[test]
fn an_authored_total_that_does_not_fit_is_a_structural_refusal_not_a_wrapped_residual() {
    // The `sum` across parties: two shares no store would hold, on a function the
    // store does not gate.
    let across_parties = reconcile(&group(
        0,
        Absorber::Platform,
        &[("a", i32::MAX), ("b", i32::MAX)],
    ))
    .expect_err("the authored total does not fit an i32");
    assert_eq!(across_parties.code(), REVSHARE_UNBALANCED);

    // And the `+` that folds the platform cut into it, which is the other operator
    // and would survive a fix to only the first.
    let onto_the_cut = reconcile(&group(i32::MAX, Absorber::Platform, &[("a", 1)]))
        .expect_err("the authored total does not fit an i32");
    assert_eq!(onto_the_cut.code(), REVSHARE_UNBALANCED);

    // And the **subtraction** the fold feeds, which is the third operator and the
    // one that survived a fix to the first two: `try_fold` admits any `authored`
    // inside `i32`, and `10_000 - i32::MIN` is not one. A `platform_cut_bp` of
    // `i32::MIN` with one zero share folds without overflowing and then panicked
    // one line down, at `FULL_ALLOCATION_BP - authored` (review 2026-08-19).
    let onto_the_residual = reconcile(&group(i32::MIN, Absorber::Platform, &[("a", 0)]))
        .expect_err("the distance from the whole does not fit an i32");
    assert_eq!(onto_the_residual.code(), REVSHARE_UNBALANCED);

    // And an ordinary group still reconciles, so the guard has not swallowed the
    // arithmetic it protects.
    assert_eq!(
        reconcile(&group(1_000, Absorber::Platform, &[("a", 9_000)]))
            .expect("an exact split")
            .sums_to(),
        FULL_ALLOCATION_BP
    );
}

/// **The residual lands on the absorber without wrapping.**
///
/// `authored` and the residual are both formed under `checked_*`, and the residual
/// is then applied to the absorber's own value with a `+`. That operand is the
/// caller's, not the residual's, so guarding the two earlier stages does not bound
/// this one: a `platform_cut_bp` at `i32::MAX` with a compensating negative share
/// folds cleanly, subtracts cleanly, leaves a 1 bp residual inside tolerance, and
/// overflows only here. It is the shape `reconcile`'s own comment names — a check
/// that moves the panic one line down rather than removing it — one stage later
/// than the comment is written beside.
///
/// The `0..=FULL_ALLOCATION_BP` range check below the absorption cannot stand in
/// for the guard: it reads the value the wrap already produced, so it refuses for
/// a distance that never existed, and a debug build panics before reaching it.
#[test]
fn the_residual_lands_on_the_absorber_without_wrapping() {
    // The platform arm.
    let onto_the_cut = reconcile(&group(
        i32::MAX,
        Absorber::Platform,
        &[("a", 9_999 - i32::MAX)],
    ))
    .expect_err("absorbing the residual does not fit an i32");
    assert_eq!(onto_the_cut.code(), REVSHARE_UNBALANCED);

    // And the party arm, which is a second operator over a second operand and
    // would survive a fix to the first.
    let onto_a_party = reconcile(&group(
        9_999 - i32::MAX,
        Absorber::Party(Party::new("a").expect("a well-formed party")),
        &[("a", i32::MAX)],
    ))
    .expect_err("absorbing the residual does not fit an i32");
    assert_eq!(onto_a_party.code(), REVSHARE_UNBALANCED);

    // And an ordinary group still absorbs, so the guard has not swallowed the
    // arithmetic it protects.
    assert_eq!(
        reconcile(&group(
            0,
            Absorber::Party(Party::new("a").expect("a well-formed party")),
            &[("a", 3333), ("b", 3333), ("c", 3333)],
        ))
        .expect("a 1 bp residual is within tolerance")
        .sums_to(),
        FULL_ALLOCATION_BP
    );
}

/// D-07's own example of what must **not** normalize: a six-way even split is
/// 9996 bp, four out, and the operator has to reconcile it.
#[test]
fn a_six_way_even_split_is_over_tolerance() {
    let err = reconcile(&group(
        0,
        Absorber::Platform,
        &[
            ("a", 1666),
            ("b", 1666),
            ("c", 1666),
            ("d", 1666),
            ("e", 1666),
            ("f", 1666),
        ],
    ))
    .expect_err("9996 bp is four basis points out");

    assert_eq!(err.code(), RESIDUAL_OVER_TOLERANCE);
}

/// The tolerance is symmetric: 1 bp either way reconciles, 2 bp does not.
#[test]
fn the_tolerance_is_exactly_one_basis_point_in_both_directions() {
    for residual in [-1_i32, 1] {
        assert!(
            reconcile(&group(1000, Absorber::Platform, &[("a", 9000 - residual)])).is_ok(),
            "a residual of {residual} bp is within tolerance"
        );
    }
    for residual in [-2_i32, 2] {
        assert_eq!(
            reconcile(&group(1000, Absorber::Platform, &[("a", 9000 - residual)]))
                .expect_err("two basis points is over tolerance")
                .code(),
            RESIDUAL_OVER_TOLERANCE
        );
    }
}

/// A group with no party rows has nothing to allocate: the shares are the
/// allocation base, and a platform cut alone is not a split.
#[test]
fn a_group_with_no_parties_is_structurally_unbalanced() {
    let err = reconcile(&group(10_000, Absorber::Platform, &[]))
        .expect_err("a group with no parties has no split");

    assert_eq!(err.code(), REVSHARE_UNBALANCED);
}

/// An absorber naming a party the group does not hold cannot absorb anything.
///
/// §5 declares no code for this, and a gear may not mint a wire code, so it
/// renders under `REVSHARE_UNBALANCED` — whose D-07 narrowing is "structural
/// malformation", which this is: no member takes the residual, so the group
/// cannot be made to sum to 10000. The gap is in the owed register (B-5).
#[test]
fn an_absorber_outside_the_group_is_structurally_unbalanced() {
    let err = reconcile(&group(
        1,
        Absorber::Party(Party::new("stranger").expect("party")),
        &[("a", 3333), ("b", 3333), ("c", 3333)],
    ))
    .expect_err("the absorber is not a party of this group");

    assert_eq!(err.code(), REVSHARE_UNBALANCED);
}

/// Normalization must not push the absorber outside the scale it is measured
/// on. A party at 0 bp cannot absorb a downward residual.
#[test]
fn an_absorber_is_never_normalized_off_the_scale() {
    let err = reconcile(&group(
        10_001,
        Absorber::Party(Party::new("a").expect("party")),
        &[("a", 0)],
    ))
    .expect_err("the absorber would go negative");

    assert_eq!(err.code(), REVSHARE_UNBALANCED);
}

// ---------------------------------------------------------------------------
// `inst-rs-sum` / D-55 — rev-share is a `sum_of_parts` property.
// ---------------------------------------------------------------------------

/// An `own_price` bundle has one amount and no per-vendor-SKU revenue to
/// allocate, so it has no declared allocation base at all.
#[test]
fn rev_share_on_an_own_price_bundle_is_refused() {
    let err = check_basis_admits_rev_share(PriceBasis::OwnPrice, 1)
        .expect_err("D-55 refuses rev-share on an own_price bundle");

    assert_eq!(err.code(), REVSHARE_BASIS_UNSUPPORTED);
}

/// An `own_price` bundle with **no** rev-share is the ordinary case and passes.
#[test]
fn an_own_price_bundle_without_rev_share_is_fine() {
    assert!(check_basis_admits_rev_share(PriceBasis::OwnPrice, 0).is_ok());
}

#[test]
fn rev_share_on_a_sum_of_parts_bundle_is_admitted() {
    assert!(check_basis_admits_rev_share(PriceBasis::SumOfParts, 2).is_ok());
}
