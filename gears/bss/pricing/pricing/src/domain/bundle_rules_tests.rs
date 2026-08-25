use super::*;
use crate::domain::bundle::{Absorber, Party, PartyShare, PriceBasis, RevShareGroup};
use crate::domain::money::CurrencyCode;
use crate::domain::plan_shape::Frequency;
use crate::domain::scope_key::Region;
use uuid::Uuid;

fn eur() -> CurrencyCode {
    CurrencyCode::new("EUR").expect("EUR")
}

fn usd() -> CurrencyCode {
    CurrencyCode::new("USD").expect("USD")
}

fn de() -> Region {
    Region::new("DE").expect("DE")
}

fn us() -> Region {
    Region::new("US").expect("US")
}

fn row(currency: &CurrencyCode, region: &Region, tax_inclusive: bool) -> CoverageRow {
    CoverageRow {
        currency: currency.clone(),
        region: region.clone(),
        tax_inclusive,
    }
}

/// A published, unphased, monthly recurring component covering one market.
fn component(id: u128, rows: Vec<CoverageRow>) -> ComponentSnapshot {
    ComponentSnapshot {
        component_plan_id: Uuid::from_u128(id),
        included_sku_id: Uuid::from_u128(id + 0x1000),
        defects: std::collections::BTreeSet::new(),
        frequency: Some(Frequency::Monthly),
        rows,
    }
}

fn composition(basis: PriceBasis, components: Vec<ComponentSnapshot>) -> BundleComposition {
    BundleComposition {
        bundle_id: Uuid::from_u128(0xb0_1d),
        basis,
        markets: vec![(eur(), de())],
        components,
        own_rows: Vec::new(),
        rev_share_groups: Vec::new(),
    }
}

fn codes(report: &crate::domain::validation::ValidationReport) -> Vec<&str> {
    report.violations.iter().map(|v| v.code.as_str()).collect()
}

// ---------------------------------------------------------------------------
// `inst-bb-declared` — the basis.
// ---------------------------------------------------------------------------

#[test]
fn a_request_with_no_basis_is_refused() {
    assert_eq!(check_basis_declared(None).unwrap_err(), BASIS_MISSING);
}

#[test]
fn a_declared_basis_passes_through() {
    assert_eq!(
        check_basis_declared(Some(PriceBasis::SumOfParts)),
        Ok(PriceBasis::SumOfParts)
    );
}

// ---------------------------------------------------------------------------
// `inst-bb-sum` — what a component may be.
// ---------------------------------------------------------------------------

#[test]
fn a_well_formed_sum_of_parts_bundle_publishes() {
    let report = validate(&composition(
        PriceBasis::SumOfParts,
        vec![component(1, vec![row(&eur(), &de(), true)])],
    ));

    assert!(
        report.is_publishable(),
        "unexpected: {:?}",
        report.violations
    );
}

#[test]
fn an_unpublished_component_blocks_publish() {
    let mut c = component(1, vec![row(&eur(), &de(), true)]);
    c.defects.insert(ComponentDefect::Unpublished);

    let report = validate(&composition(PriceBasis::SumOfParts, vec![c]));

    assert!(codes(&report).contains(&COMPONENT_UNPUBLISHED));
}

/// Flat composition at launch: nesting is a named Future gate, and re-composition
/// is re-validated so a cycle can never form.
#[test]
fn a_component_that_is_itself_a_bundle_blocks_publish() {
    let mut c = component(1, vec![row(&eur(), &de(), true)]);
    c.defects.insert(ComponentDefect::IsBundlePlan);

    let report = validate(&composition(PriceBasis::SumOfParts, vec![c]));

    assert!(codes(&report).contains(&COMPONENT_IS_BUNDLE));
}

/// L-4: which phase's rows sum, and whether a bundle subscription runs the
/// component's phase schedule, are undecided semantics — a named Future gate.
#[test]
fn a_phased_component_blocks_publish() {
    let mut c = component(1, vec![row(&eur(), &de(), true)]);
    c.defects.insert(ComponentDefect::Phased);

    let report = validate(&composition(PriceBasis::SumOfParts, vec![c]));

    assert!(codes(&report).contains(&COMPONENT_PHASED));
}

/// A `sum_of_parts` bundle with no components sums nothing — the reference set
/// is the whole of what the catalog persists for it.
#[test]
fn a_sum_of_parts_bundle_with_no_components_blocks_publish() {
    let report = validate(&composition(PriceBasis::SumOfParts, Vec::new()));

    assert!(codes(&report).contains(&COMPONENT_UNPUBLISHED));
}

// ---------------------------------------------------------------------------
// `inst-bc-coverage` / `inst-bc-fail` — the per-market walk.
// ---------------------------------------------------------------------------

/// The failure names the component **and** the market, which is `inst-bc-fail`'s
/// whole content: an operator remediating coverage has to know which row to
/// author where.
#[test]
fn a_component_missing_a_sold_market_blocks_publish_naming_both() {
    let mut c = composition(
        PriceBasis::SumOfParts,
        vec![component(1, vec![row(&eur(), &de(), true)])],
    );
    c.markets.push((usd(), us()));

    let report = validate(&c);

    let violation = report
        .violations
        .iter()
        .find(|v| v.code == CURRENCY_NOT_COVERED)
        .expect("the market must be reported uncovered");
    assert!(
        violation.subject.contains(&Uuid::from_u128(1).to_string()),
        "the component must be named: {}",
        violation.subject
    );
    assert!(
        violation.detail.contains("USD") && violation.detail.contains("US"),
        "the market must be named: {}",
        violation.detail
    );
}

/// A market named twice is one authoring mistake, and the report says so once.
///
/// **Reachable, not defensive**: `markets` arrives from the publish request body
/// and `bundles::markets_of` maps it without deduplicating, so a caller repeating
/// a pair reaches this rule. The joint walk this used to be asked `.any()` per
/// listed pair and therefore reported the same missing market once per
/// repetition, per component — N violations for one row nobody authored.
///
/// The deduplication is not a filter added here. It comes from `uncovered_pairs`
/// being a **set** difference, which is the predicate D-211 makes these two
/// planes share: the add-on arm has always answered over sets, and this is the
/// bundle arm arriving at the same answer by the same route rather than by a
/// second implementation that agrees today.
#[test]
fn a_market_named_twice_is_reported_once() {
    // Component 1 covers neither the repeated market (one `CURRENCY_NOT_COVERED`)
    // nor agrees with anyone on tax; components 2 and 3 both cover it and
    // disagree with each other (one `BUNDLE_TAX_BASIS_MIXED`). **Both rules are
    // counted**, because both walk `composition.markets` and only one of them
    // used to build a set out of it first: the tax rule reported the repeated
    // pair once per repetition until 2026-08-20, which is the fault the coverage
    // rule's own fix note describes and this case was assumed to pin.
    let mut c = composition(
        PriceBasis::SumOfParts,
        vec![
            component(1, vec![row(&eur(), &de(), true)]),
            component(2, vec![row(&eur(), &de(), true), row(&usd(), &us(), true)]),
            component(3, vec![row(&eur(), &de(), true), row(&usd(), &us(), false)]),
        ],
    );
    c.markets.push((usd(), us()));
    c.markets.push((usd(), us()));

    let report = validate(&c);

    let uncovered = report
        .violations
        .iter()
        .filter(|v| v.code == CURRENCY_NOT_COVERED)
        .count();
    assert_eq!(
        uncovered, 1,
        "one missing market, one violation — got {:?}",
        report.violations
    );

    let mixed = report
        .violations
        .iter()
        .filter(|v| v.code == BUNDLE_TAX_BASIS_MIXED)
        .count();
    assert_eq!(
        mixed, 1,
        "one market with two bases, one violation — got {:?}",
        report.violations
    );
}

/// Two components, two markets, both covered — the AC's two-vendor case.
#[test]
fn every_component_covering_every_market_publishes() {
    let mut c = composition(
        PriceBasis::SumOfParts,
        vec![
            component(1, vec![row(&eur(), &de(), true), row(&usd(), &us(), false)]),
            component(2, vec![row(&eur(), &de(), true), row(&usd(), &us(), false)]),
        ],
    );
    c.markets.push((usd(), us()));

    let report = validate(&c);

    assert!(
        report.is_publishable(),
        "unexpected: {:?}",
        report.violations
    );
}

// ---------------------------------------------------------------------------
// `inst-bc-frequency` — recurring components only.
// ---------------------------------------------------------------------------

#[test]
fn a_monthly_and_an_annual_component_cannot_sum_onto_one_invoice() {
    let mut annual = component(2, vec![row(&eur(), &de(), true)]);
    annual.frequency = Some(Frequency::Annual);

    let report = validate(&composition(
        PriceBasis::SumOfParts,
        vec![component(1, vec![row(&eur(), &de(), true)]), annual],
    ));

    assert!(codes(&report).contains(&FREQUENCY_MISMATCH));
}

/// The refusal names every side with its own owners, and says the same thing
/// whatever order the components arrive in.
///
/// Both halves are the faults `check_tax_basis`'s doc records as the cost of a
/// first-seen referent, and they are the two this rule still paid.
///
/// **The outlier is listed first on purpose.** Under a referent taken from the
/// first recurring component the four conforming owners are the ones reported
/// and the real outlier appears only inside a detail string — an operator reading
/// the report is sent to change the four plans that agree.
///
/// **And a composition is not its component order.** The same five components in
/// two orders are one composition and owe one report, which is the argument this
/// module already makes for its market set in `check_coverage` and
/// `check_tax_basis`.
#[test]
fn a_frequency_mismatch_names_every_side_and_does_not_depend_on_component_order() {
    let quarterly = |id: u128| {
        let mut c = component(id, vec![row(&eur(), &de(), true)]);
        c.frequency = Some(Frequency::Quarterly);
        c
    };

    let outlier_first = validate(&composition(
        PriceBasis::SumOfParts,
        vec![
            quarterly(9),
            component(1, vec![row(&eur(), &de(), true)]),
            component(2, vec![row(&eur(), &de(), true)]),
            component(3, vec![row(&eur(), &de(), true)]),
            component(4, vec![row(&eur(), &de(), true)]),
        ],
    ));

    let mismatches: Vec<_> = outlier_first
        .violations
        .iter()
        .filter(|v| v.code == FREQUENCY_MISMATCH)
        .collect();
    assert_eq!(
        mismatches.len(),
        1,
        "one composition, one refusal: {:?}",
        outlier_first.violations
    );

    // Every side, each beside its own owners — so the operator can read what the
    // bundle's frequency *is* as well as which plan disagrees.
    let detail = &mismatches[0].detail;
    let quarterly_id = Uuid::from_u128(9).to_string();
    for id in [1_u128, 2, 3, 4] {
        assert!(
            detail.contains(&Uuid::from_u128(id).to_string()),
            "the monthly side must name plan {id}: {detail}"
        );
    }
    assert!(
        detail.contains(&quarterly_id),
        "the quarterly side must name the outlier: {detail}"
    );

    let outlier_last = validate(&composition(
        PriceBasis::SumOfParts,
        vec![
            component(1, vec![row(&eur(), &de(), true)]),
            component(2, vec![row(&eur(), &de(), true)]),
            component(3, vec![row(&eur(), &de(), true)]),
            component(4, vec![row(&eur(), &de(), true)]),
            quarterly(9),
        ],
    ));
    let reordered: Vec<_> = outlier_last
        .violations
        .iter()
        .filter(|v| v.code == FREQUENCY_MISMATCH)
        .map(|v| (v.subject.clone(), v.detail.clone()))
        .collect();
    assert_eq!(
        reordered,
        mismatches
            .iter()
            .map(|v| (v.subject.clone(), v.detail.clone()))
            .collect::<Vec<_>>(),
        "one composition in two orders owes one report"
    );
}

/// L-8: a usage-only component carries no `frequency` and is outside the rule —
/// its charges rate per its own rows.
#[test]
fn a_usage_only_component_is_outside_the_frequency_rule() {
    let mut usage = component(2, vec![row(&eur(), &de(), true)]);
    usage.frequency = None;

    let report = validate(&composition(
        PriceBasis::SumOfParts,
        vec![component(1, vec![row(&eur(), &de(), true)]), usage],
    ));

    assert!(
        report.is_publishable(),
        "unexpected: {:?}",
        report.violations
    );
}

/// And a bundle of **only** usage components has no frequency to match at all.
#[test]
fn a_bundle_of_only_usage_components_publishes() {
    let mut usage = component(1, vec![row(&eur(), &de(), true)]);
    usage.frequency = None;

    let report = validate(&composition(PriceBasis::SumOfParts, vec![usage]));

    assert!(
        report.is_publishable(),
        "unexpected: {:?}",
        report.violations
    );
}

// ---------------------------------------------------------------------------
// `inst-bc-taxbasis` / D-119 — one display basis per bundle-market.
// ---------------------------------------------------------------------------

#[test]
fn a_market_whose_components_disagree_on_tax_basis_blocks_publish() {
    let report = validate(&composition(
        PriceBasis::SumOfParts,
        vec![
            component(1, vec![row(&eur(), &de(), true)]),
            component(2, vec![row(&eur(), &de(), false)]),
        ],
    ));

    let violation = report
        .violations
        .iter()
        .find(|v| v.code == BUNDLE_TAX_BASIS_MIXED)
        .expect("a mixed market must block");
    // **Both** owners, not only the one that differs from whichever the walk
    // reached first (review Z3-9). An operator told "component 2 disagrees" still
    // has to go and find what the market's basis is; naming each owner beside its
    // own value answers that in one read, which is the argument `MarketBasisUniform`
    // and `ProrationContractMarketUniform` both carry in writing.
    for owner in [Uuid::from_u128(1), Uuid::from_u128(2)] {
        assert!(
            violation.detail.contains(&owner.to_string()),
            "every side of the mixed market must be named, not only the divergent one: {}",
            violation.detail
        );
    }
    assert!(
        violation.detail.contains("tax_inclusive=true")
            && violation.detail.contains("tax_inclusive=false"),
        "each side is rendered beside its own basis: {}",
        violation.detail
    );
}

/// The case the first-seen referent got backwards, and the reason Z3-9 is a
/// message defect rather than a missed refusal.
///
/// Four conforming components and one outlier, with **the outlier first**. Under
/// the old walk the first row set the referent, so the refusal named the four
/// conforming owners as "divergent" and stayed silent about the one that was.
/// The refusal itself fired either way, which is why no existing case could see
/// it.
#[test]
fn the_outlier_arriving_first_does_not_make_the_conforming_rows_the_divergent_ones() {
    let report = validate(&composition(
        PriceBasis::SumOfParts,
        vec![
            component(1, vec![row(&eur(), &de(), false)]),
            component(2, vec![row(&eur(), &de(), true)]),
            component(3, vec![row(&eur(), &de(), true)]),
            component(4, vec![row(&eur(), &de(), true)]),
            component(5, vec![row(&eur(), &de(), true)]),
        ],
    ));

    let violation = report
        .violations
        .iter()
        .find(|v| v.code == BUNDLE_TAX_BASIS_MIXED)
        .expect("a mixed market must block whichever side arrives first");
    assert!(
        violation
            .detail
            .contains(&format!("tax_inclusive=false: {}", Uuid::from_u128(1))),
        "the lone outlier is named on its own side, alone: {}",
        violation.detail
    );
    for conforming in 2..=5u128 {
        assert!(
            violation
                .detail
                .contains(&Uuid::from_u128(conforming).to_string()),
            "and every conforming owner is named on the other side rather than being reported \
             as the divergence: {}",
            violation.detail
        );
    }
}

/// An owner whose **own** two rows disagree is on both sides.
///
/// Keyed by owner, the old map's second insert overwrote the first, so exactly
/// one of the two bases reached the message and which one depended on row order.
#[test]
fn one_owner_holding_both_bases_is_named_on_both_sides() {
    let report = validate(&composition(
        PriceBasis::SumOfParts,
        vec![component(
            7,
            vec![row(&eur(), &de(), true), row(&eur(), &de(), false)],
        )],
    ));

    let violation = report
        .violations
        .iter()
        .find(|v| v.code == BUNDLE_TAX_BASIS_MIXED)
        .expect("one component mixing bases within a market is still a mixed market");
    let owner = Uuid::from_u128(7).to_string();
    assert!(
        violation
            .detail
            .contains(&format!("tax_inclusive=false: {owner}"))
            && violation
                .detail
                .contains(&format!("tax_inclusive=true: {owner}")),
        "the owner appears on both sides, because that is the fact: {}",
        violation.detail
    );
}

/// D-119's own accepting case: all-inclusive EU beside all-exclusive US is two
/// markets, each uniform, and publishes.
#[test]
fn one_basis_per_market_publishes_even_when_the_markets_differ() {
    let mut c = composition(
        PriceBasis::SumOfParts,
        vec![
            component(1, vec![row(&eur(), &de(), true), row(&usd(), &us(), false)]),
            component(2, vec![row(&eur(), &de(), true), row(&usd(), &us(), false)]),
        ],
    );
    c.markets.push((usd(), us()));

    let report = validate(&c);

    assert!(
        report.is_publishable(),
        "unexpected: {:?}",
        report.violations
    );
}

/// For `own_price` the bundle's **own** rows are in the uniformity set too.
#[test]
fn an_own_price_bundles_own_row_can_mix_the_market() {
    let mut c = composition(
        PriceBasis::OwnPrice,
        vec![component(1, vec![row(&eur(), &de(), true)])],
    );
    c.own_rows = vec![row(&eur(), &de(), false)];

    let report = validate(&c);

    assert!(codes(&report).contains(&BUNDLE_TAX_BASIS_MIXED));
}

// ---------------------------------------------------------------------------
// `inst-rs-sum` / D-55 and `inst-rs-residual` / D-07, through the validator.
// ---------------------------------------------------------------------------

fn one_group(shares: &[(&str, i32)], cut: i32) -> RevShareGroup {
    RevShareGroup {
        vendor_sku_id: Uuid::from_u128(0x_5e_11),
        platform_cut_bp: cut,
        residual_absorber: Absorber::Platform,
        parties: shares
            .iter()
            .map(|(n, bp)| PartyShare {
                party: Party::new(n).expect("party"),
                share_bp: *bp,
            })
            .collect(),
    }
}

#[test]
fn rev_share_on_an_own_price_bundle_blocks_publish() {
    let mut c = composition(
        PriceBasis::OwnPrice,
        vec![component(1, vec![row(&eur(), &de(), true)])],
    );
    c.own_rows = vec![row(&eur(), &de(), true)];
    c.rev_share_groups = vec![one_group(&[("a", 9000)], 1000)];

    let report = validate(&c);

    assert!(codes(&report).contains(&REVSHARE_BASIS_UNSUPPORTED));
}

#[test]
fn a_group_over_tolerance_blocks_publish() {
    let mut c = composition(
        PriceBasis::SumOfParts,
        vec![component(1, vec![row(&eur(), &de(), true)])],
    );
    c.rev_share_groups = vec![one_group(&[("a", 5000), ("b", 4000)], 0)];

    let report = validate(&c);

    assert!(codes(&report).contains(&RESIDUAL_OVER_TOLERANCE));
}

/// The per-group reconciliation runs over each group independently — the AC's
/// three-component two-vendor case.
#[test]
fn each_group_reconciles_independently() {
    let mut c = composition(
        PriceBasis::SumOfParts,
        vec![component(1, vec![row(&eur(), &de(), true)])],
    );
    let mut second = one_group(&[("c", 8000)], 2000);
    second.vendor_sku_id = Uuid::from_u128(0x_5e_22);
    c.rev_share_groups = vec![one_group(&[("a", 4500), ("b", 4500)], 1000), second];

    let report = validate(&c);

    assert!(
        report.is_publishable(),
        "unexpected: {:?}",
        report.violations
    );
}

/// The report is aggregate: one pass tells the operator everything wrong.
#[test]
fn every_failure_of_one_composition_is_reported_in_one_pass() {
    let mut phased = component(2, vec![row(&eur(), &de(), false)]);
    phased.defects.insert(ComponentDefect::Phased);
    phased.frequency = Some(Frequency::Annual);

    let report = validate(&composition(
        PriceBasis::SumOfParts,
        vec![component(1, vec![row(&eur(), &de(), true)]), phased],
    ));

    let found = codes(&report);
    for expected in [COMPONENT_PHASED, FREQUENCY_MISMATCH, BUNDLE_TAX_BASIS_MIXED] {
        assert!(
            found.contains(&expected),
            "{expected} must be reported alongside the others, got {found:?}"
        );
    }
}
