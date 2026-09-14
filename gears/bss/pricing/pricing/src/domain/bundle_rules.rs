//! `BundleValidator` — the publish-time composition rules of
//! `design/08-bundles.md` §3: `inst-bb-declared`, `inst-bb-sum`, `inst-bb-own`,
//! `inst-bc-coverage`, `inst-bc-frequency`, `inst-bc-fail`, `inst-bc-taxbasis`
//! and the `inst-rs-*` rules' entry into the same report.
//!
//! Where [`crate::domain::bundle`] is arithmetic over **one** rev-share group,
//! this module is about a whole composition: every component, every sold market,
//! and the cross-component properties that no single row can carry.
//!
//! # The input is a snapshot, and that is what makes these rules pure
//!
//! [`BundleComposition`] holds facts the caller has already resolved — is this
//! component plan published, is it itself a bundle-type plan, does it carry an
//! authored phase schedule, what rows does it have. None of them is derivable
//! here, and all of them are reads. §4.2's contract requires a rule to be **pure
//! with respect to the state it is handed**, because the same rule set runs twice
//! — as a pre-check at submit and again inside the publish-commit transaction,
//! where the world has moved. A validator that fetched its own inputs would be
//! answering two different questions on the two runs.
//!
//! # The coverage set is narrowed before it arrives, deliberately
//!
//! `inst-bc-coverage` evaluates over `priceEligibility = all_subscriptions`
//! (`cohort = none`) rows **only**: grandfathered generations (ADR-0002) are
//! never coverage candidates, and `new_subscriptions_only` rows are not either —
//! bundle composition demands the durable base, and a new-only promo row expires
//! with its intent. That narrowing is the **caller's**, applied when it builds
//! [`ComponentSnapshot::rows`], for the reason above: it is a filter over stored
//! rows, and re-deriving "which rows count" here would be a second answer to a
//! question the read already settled. [`CoverageRow`] carries only what the rules
//! compare, which is what keeps the two from drifting.
//!
//! # Slice 4's debt to this module, and how it was paid
//!
//! `inst-bc-coverage` says the **currency** axis delegates to Slice 4's
//! `CurrencyBindingChecker` (case ii) while the `region` axis is this
//! validator's own extension of the same rule. That checker did not exist when
//! this walk was written, so both axes were checked here and the delegation was
//! reported as owed (D-211).
//!
//! Slice 4 landed it, and the delegation is now made — **to the predicate, not
//! to the walk**, and the distinction is the whole of what D-211 buys. D-95
//! moved case (i) from *currency* to the `(currency, region)` **pair**, so there
//! is no currency axis left to hand over on its own: what the two planes can
//! honestly share is the question *which sold markets is this thing not covering*,
//! which is [`crate::domain::currency_binding::uncovered_pairs`]. The rule stays
//! here — which markets are sold, which rows count, what the violation says —
//! and only the set difference is shared.
//!
//! Sharing it changed an answer rather than merely relocating one:
//! `composition.markets` is a `Vec` off the request body with no deduplication
//! anywhere on the route, so the old `.any()` walk reported a repeated pair once
//! per repetition. A set difference reports it once
//! ([`a_market_named_twice_is_reported_once`](bundle_rules_tests)).
//!
//! # `inst-bc-sellability` is not here
//!
//! The Slice-7 gate evaluating a bundle as the conjunction over its components
//! is a rule about **an instant** — predicates (1)–(5) at `t` — and this module
//! has no clock and no window set. It belongs beside
//! [`crate::domain::sellability`], whose predicates it reuses, and it is called
//! out here so the absence reads as placement rather than omission.

use std::collections::{BTreeMap, BTreeSet};

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::bundle::{
    PriceBasis, RESIDUAL_OVER_TOLERANCE, REVSHARE_BASIS_UNSUPPORTED, REVSHARE_UNBALANCED,
    RevShareGroup, check_basis_admits_rev_share, reconcile,
};
use crate::domain::currency_binding::{Market, uncovered_pairs};
use crate::domain::money::CurrencyCode;
use crate::domain::plan_shape::Frequency;
use crate::domain::scope_key::Region;
use crate::domain::validation::ValidationReport;

/// No price basis declared (§5, **422 architectural**; `inst-bb-declared`).
///
/// **Reachable only from a request**, and the type system is why: `price_basis`
/// is `NOT NULL` on `pricing_bundle` and [`PriceBasis`] is a closed enum, so a
/// *stored* bundle always has one. The code therefore belongs to the authoring
/// edge, which is where [`check_basis_declared`] is called from, and not to the
/// composition walk below — a validator taking `Option<PriceBasis>` would be
/// carrying an arm no stored row can reach.
pub const BASIS_MISSING: &str = "BASIS_MISSING";

/// A referenced component plan has not published (§5; `inst-bb-sum`).
pub const COMPONENT_UNPUBLISHED: &str = "COMPONENT_UNPUBLISHED";

/// A referenced component plan is itself a `bundle`-type plan (§5;
/// `inst-bb-sum`).
///
/// Flat composition at launch; nesting is a named Future gate. Re-composition is
/// re-validated, so a cycle can never form — which is why this is a per-component
/// predicate rather than a graph walk.
pub const COMPONENT_IS_BUNDLE: &str = "COMPONENT_IS_BUNDLE";

/// A referenced component plan carries an authored phase schedule (§5;
/// `inst-bb-sum`, L-4).
///
/// Which phase's rows sum, and whether a bundle subscription runs the component's
/// phase schedule, are undecided semantics — the D-53 posture, a named Future
/// gate rather than a defect.
pub const COMPONENT_PHASED: &str = "COMPONENT_PHASED";

/// A component has no covering published row for a sold `(currency, region)`
/// (§5; `inst-bc-coverage`, `inst-bc-fail`).
///
/// **Re-exported, not re-declared.** §5 gives one code to all three enumerated
/// configurations — this one and `currency_binding`'s two — because the
/// operator's remedy is the same in each, and `infra::metrics` depends on the two
/// planes raising a string that is *equal*: it imports this name and matches
/// violations with it while its own comments record in prose that
/// `currency_binding` raises "the same string". Two `pub const`s made that a
/// coincidence between two literals rather than a fact about one, and only one of
/// the two had a byte-for-byte spelling pin
/// (`currency_binding_tests.rs:140`) — this copy had none in the unit layer at
/// all. One declaration, at the module where §5's shared-code argument is
/// written down.
pub use crate::domain::currency_binding::CURRENCY_NOT_COVERED;

/// Recurring components disagree on `frequency` (§5; `inst-bc-frequency`).
pub const FREQUENCY_MISMATCH: &str = "FREQUENCY_MISMATCH";

/// The rows of one sold `(currency, region)` disagree on `tax_inclusive`
/// (§5; `inst-bc-taxbasis`, D-119).
pub const BUNDLE_TAX_BASIS_MIXED: &str = "BUNDLE_TAX_BASIS_MIXED";

/// One published row of one component, reduced to what the composition rules
/// compare.
///
/// Deliberately not a [`PriceRecord`](crate::domain::price_record::PriceRecord):
/// these rules range over *another plan's* rows, and carrying the whole record
/// would invite a rule to read a field the coverage narrowing has already made
/// meaningless here (a `cohort`, a `priceEligibility` — the caller filtered on
/// both before building this).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageRow {
    /// The row's currency axis.
    pub currency: CurrencyCode,
    /// The row's region axis.
    pub region: Region,
    /// The row's tax display basis (D-110's per-row column).
    pub tax_inclusive: bool,
}

/// A reason a referenced component cannot be composed, as the caller's read
/// found it.
///
/// A closed set rather than three booleans on [`ComponentSnapshot`], and not
/// only because `clippy::struct_excessive_bools` says so: each member is one
/// wire code §5 declares, so enumerating them here puts the code, the detail and
/// the fact in one place. A fourth disqualifier becomes a compile error in
/// [`ComponentDefect::code`] rather than a silently unreported state.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComponentDefect {
    /// The component plan has not published.
    Unpublished,
    /// The component plan is itself a `bundle`-type plan (flat composition at
    /// launch).
    IsBundlePlan,
    /// The component plan carries a phase schedule beyond the D-19 implicit
    /// terminal phase (L-4, a named Future gate).
    Phased,
}

impl ComponentDefect {
    /// Every defect, in §5's order.
    pub const ALL: &'static [Self] = &[Self::Unpublished, Self::IsBundlePlan, Self::Phased];

    /// The wire code §5 declares for this defect.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unpublished => COMPONENT_UNPUBLISHED,
            Self::IsBundlePlan => COMPONENT_IS_BUNDLE,
            Self::Phased => COMPONENT_PHASED,
        }
    }

    /// What the operator is told, naming the plan at fault.
    #[must_use]
    pub fn detail(self, component_plan_id: Uuid) -> String {
        match self {
            Self::Unpublished => format!(
                "component plan {component_plan_id} has not published; a bundle may only \
                 reference published components"
            ),
            Self::IsBundlePlan => format!(
                "component plan {component_plan_id} is itself a bundle-type plan; composition \
                 is flat at launch"
            ),
            Self::Phased => format!(
                "component plan {component_plan_id} carries an authored phase schedule; phased \
                 components are a named Future gate"
            ),
        }
    }
}

/// One referenced component, as the publish-time walk sees it.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentSnapshot {
    /// The component's **plan** (B1), which is what the composition references.
    pub component_plan_id: Uuid,
    /// The registry SKU it publishes under.
    pub included_sku_id: Uuid,
    /// Everything the caller's read found disqualifying about this component.
    /// Empty is the ordinary case.
    pub defects: BTreeSet<ComponentDefect>,
    /// Its recurring frequency, or `None` for a **usage-only** component.
    ///
    /// `None` is not "unknown": L-8 puts usage-only components outside
    /// `inst-bc-frequency` by construction, because their charges rate per their
    /// own rows rather than summing onto the bundle's recurring line set.
    pub frequency: Option<Frequency>,
    /// Its coverage-eligible published rows — narrowed by the caller, see the
    /// module doc.
    pub rows: Vec<CoverageRow>,
}

/// A whole bundle composition at publish time.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleComposition {
    /// The bundle this is the composition of.
    pub bundle_id: Uuid,
    /// Its declared basis.
    pub basis: PriceBasis,
    /// The `(currency, region)` markets the bundle sells in.
    pub markets: Vec<(CurrencyCode, Region)>,
    /// Its referenced components.
    pub components: Vec<ComponentSnapshot>,
    /// The bundle's **own** rows — `own_price` only, empty for `sum_of_parts`
    /// (`inst-bb-rowless`).
    pub own_rows: Vec<CoverageRow>,
    /// Its rev-share groups, one per included vendor SKU.
    pub rev_share_groups: Vec<RevShareGroup>,
}

/// `inst-bb-declared`: the basis MUST be declared.
///
/// # Errors
/// [`BASIS_MISSING`] when the request carried none. See that constant for why
/// this is a separate entry point rather than an arm of [`validate`].
pub const fn check_basis_declared(basis: Option<PriceBasis>) -> Result<PriceBasis, &'static str> {
    match basis {
        Some(basis) => Ok(basis),
        None => Err(BASIS_MISSING),
    }
}

/// Run every composition rule, collecting the whole report.
///
/// Aggregate rather than fail-fast, per §4.2: one pass tells the operator
/// everything that is wrong, which is what makes a composition remediable in one
/// edit instead of in as many edits as it has faults.
#[must_use]
pub fn validate(composition: &BundleComposition) -> ValidationReport {
    let mut report = ValidationReport::default();
    check_components(composition, &mut report);
    check_coverage(composition, &mut report);
    check_frequency(composition, &mut report);
    check_tax_basis(composition, &mut report);
    check_rev_share(composition, &mut report);
    report
}

/// `inst-bb-sum`: what a component may be.
fn check_components(composition: &BundleComposition, report: &mut ValidationReport) {
    // A `sum_of_parts` bundle with no components sums nothing. It is reported as
    // an unpublished-component failure rather than under a code of its own
    // because §5 declares none for it, and the operator's remedy is the same:
    // reference a published component plan.
    if composition.basis == PriceBasis::SumOfParts && composition.components.is_empty() {
        report.violate(
            COMPONENT_UNPUBLISHED,
            composition.bundle_id.to_string(),
            "a sum_of_parts bundle references no component plan: there is nothing to sum",
        );
    }

    for component in &composition.components {
        let subject = component.component_plan_id.to_string();
        // Ordered by the set, which is `ComponentDefect::ALL`'s order, so a
        // component with several defects reports them the same way every time.
        for defect in &component.defects {
            report.violate(
                defect.code(),
                &subject,
                defect.detail(component.component_plan_id),
            );
        }
    }
}

/// `inst-bc-coverage` + `inst-bc-fail`: every component covers every sold market,
/// and a failure names both.
fn check_coverage(composition: &BundleComposition, report: &mut ValidationReport) {
    // D-211's shared predicate, hoisted once: the sold set is a property of the
    // composition and re-deriving it per component would be the second answer
    // this delegation exists to prevent.
    let sold: BTreeSet<Market> = composition.markets.iter().cloned().collect();
    for component in &composition.components {
        let covered: BTreeSet<Market> = component
            .rows
            .iter()
            .map(|row| (row.currency.clone(), row.region.clone()))
            .collect();
        // Ordered by the set difference rather than by the request's market
        // order, which is the honest direction: two callers listing one market
        // set in two orders describe one composition and should read one report.
        for (currency, region) in uncovered_pairs(&sold, &covered) {
            report.violate(
                CURRENCY_NOT_COVERED,
                component.component_plan_id.to_string(),
                format!(
                    "component plan {} has no covering published row for ({}, {})",
                    component.component_plan_id,
                    currency.as_str(),
                    region.as_str()
                ),
            );
        }
    }
}

/// `inst-bc-frequency`: **recurring** components must match.
///
/// Usage-only components carry no frequency and are outside the rule (L-8).
///
/// Grouped by frequency and **not** measured against a first-seen referent, for
/// the reason [`check_tax_basis`] states below at length — it is the same family
/// of rule and it paid both of the same faults. With the outlier listed first,
/// one quarterly component ahead of four monthly ones named the four conforming
/// components as the divergent ones and left the real outlier inside a detail
/// string. And a referent taken from the walk's first arrival makes the report a
/// function of component order, when a composition is not its component order —
/// the argument [`check_coverage`] makes about its market set, one rule up.
///
/// One refusal for the composition, subject the bundle, every side rendered
/// beside its own owners: an operator told "these disagree" still has to find
/// what the bundle's frequency *is*, and this answers that in one read.
fn check_frequency(composition: &BundleComposition, report: &mut ValidationReport) {
    // Keyed by the frequency, holding the owners on that side — `check_tax_basis`'s
    // shape, and a `BTreeSet` so the rendering is stable.
    let mut by_frequency: BTreeMap<Frequency, BTreeSet<String>> = BTreeMap::new();
    for component in &composition.components {
        let Some(frequency) = component.frequency else {
            continue;
        };
        by_frequency
            .entry(frequency)
            .or_default()
            .insert(component.component_plan_id.to_string());
    }

    // One side is uniform and zero sides is a usage-only composition; the refusal
    // needs two, which is `check_tax_basis`'s predicate.
    if by_frequency.len() < 2 {
        return;
    }
    let sides: Vec<String> = by_frequency
        .iter()
        .map(|(frequency, owners)| {
            format!(
                "{frequency:?}: {}",
                owners.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        })
        .collect();
    report.violate(
        FREQUENCY_MISMATCH,
        composition.bundle_id.to_string(),
        format!(
            "the bundle's recurring components disagree about billing frequency — {}. Recurring \
             components cannot sum onto one invoice line set (`inst-bc-frequency`)",
            sides.join("; ")
        ),
    );
}

/// `inst-bc-taxbasis` / D-119: one tax display basis per bundle-market.
///
/// The row set is `inst-bc-coverage`'s — the same narrowed set, which is what
/// keeps a grandfathered generation of a component out of the check (D-132's
/// scoping of the D-110 sibling, inherited here through the narrowing). For
/// `own_price` the bundle's own rows are in it too, because they sell on the same
/// invoice.
///
/// The **reverse guard** D-119 also requires — a component re-publish whose basis
/// change would mix a referencing bundle's market — is not here: it is a rule
/// about *another* plan's publish, and it needs the set of bundles referencing
/// that component, which is a read this pure walk does not have. It belongs to
/// the component's own publish path and is owed.
///
/// # Every side is rendered, not the ones that differ from whichever came first
///
/// Not grouped by a **first-seen referent**, which collects only the owners whose
/// basis differs from the first row the walk happens to reach. The other two
/// members of this family of three carry a written argument for the opposite:
/// `MarketBasisUniform`
/// (`tax_display.rs`, D-110 — this rule's direct sibling one plane over) renders
/// both sides because *"§5 requires the refusal to name the divergent rows"*, and
/// `ProrationContractMarketUniform` (`contracts.rs`, D-123) states the general
/// reason: an operator told "these two disagree" still has to find what the
/// market's contract *is*, and rendering each owner beside its own value answers
/// that in one read.
///
/// Two concrete faults went with the referent, neither of them a missed refusal —
/// the rule fired on exactly the right input either way — and both of them
/// message defects an operator pays for:
///
/// - **If the outlier is first, every conforming owner is named as divergent.**
///   One tax-exclusive component ahead of four tax-inclusive ones reported the
///   four.
/// - **An owner with two rows disagreeing internally reported only the last**,
///   because `divergent` was keyed by owner and the second insert overwrote the
///   first. Under the grouping below such an owner appears on **both** sides,
///   which is the fact.
fn check_tax_basis(composition: &BundleComposition, report: &mut ValidationReport) {
    // A **set**, as `check_coverage` builds one, and for the same reason: nothing
    // deduplicates `composition.markets` — not `bundles::markets_of`, not the
    // publish route — so a caller listing one `(currency, region)` twice got two
    // byte-identical refusals for one authoring mistake. That is the defect the
    // module doc records as fixed for the coverage rule ("the old `.any()` walk
    // reported a repeated pair once per repetition"); it was still here, and
    // unpinned, because `a_market_named_twice_is_reported_once` counted only
    // `CURRENCY_NOT_COVERED`.
    let markets: BTreeSet<Market> = composition.markets.iter().cloned().collect();
    for (currency, region) in &markets {
        // Keyed by the basis, holding the owners on that side — `BasesInMarket`'s
        // shape in `tax_display.rs`, and a `BTreeSet` so an owner contributing
        // several rows on one basis is named once and the order is stable.
        let mut by_basis: BTreeMap<bool, BTreeSet<String>> = BTreeMap::new();

        let own = composition
            .own_rows
            .iter()
            .map(|row| (composition.bundle_id, row));
        let component_rows = composition
            .components
            .iter()
            .flat_map(|c| c.rows.iter().map(move |row| (c.component_plan_id, row)));

        for (owner, row) in component_rows.chain(own) {
            if &row.currency != currency || &row.region != region {
                continue;
            }
            by_basis
                .entry(row.tax_inclusive)
                .or_default()
                .insert(owner.to_string());
        }

        // One side is uniform and zero sides is an empty market; the refusal
        // needs two, which is the same predicate `MarketBasisUniform` uses.
        if by_basis.len() < 2 {
            continue;
        }
        let sides: Vec<String> = by_basis
            .iter()
            .map(|(inclusive, owners)| {
                format!(
                    "tax_inclusive={inclusive}: {}",
                    owners.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            })
            .collect();
        report.violate(
            BUNDLE_TAX_BASIS_MIXED,
            format!("{}/{}", currency.as_str(), region.as_str()),
            format!(
                "market ({}, {}) mixes tax display bases — {}. An invoice is one document and \
                 `tax_inclusive` is a display basis, so the bundle's lines cannot be rendered \
                 coherently side by side (D-119)",
                currency.as_str(),
                region.as_str(),
                sides.join("; ")
            ),
        );
    }
}

/// `inst-rs-sum` (D-55) and `inst-rs-residual` (D-07), into the same report.
fn check_rev_share(composition: &BundleComposition, report: &mut ValidationReport) {
    if check_basis_admits_rev_share(composition.basis, composition.rev_share_groups.len()).is_err()
    {
        report.violate(
            REVSHARE_BASIS_UNSUPPORTED,
            composition.bundle_id.to_string(),
            "rev-share is authorable on sum_of_parts bundles only: an own_price bundle has no \
             per-vendor-SKU allocation base (D-55)",
        );
        // No point reconciling groups that may not exist at all — the operator's
        // remedy is to remove them or change the basis, and a second refusal per
        // group would bury it.
        return;
    }

    for group in &composition.rev_share_groups {
        let Err(refusal) = reconcile(group) else {
            continue;
        };
        let subject = group.vendor_sku_id.to_string();
        let detail = match &refusal {
            crate::domain::bundle::RevShareRefusal::ResidualOverTolerance {
                residual_bp, ..
            } => {
                format!(
                    "vendor SKU {} is {residual_bp} bp from an exact split; the authoring \
                     tolerance is 1 bp (D-07)",
                    group.vendor_sku_id
                )
            }
            crate::domain::bundle::RevShareRefusal::Unbalanced { detail, .. } => detail.clone(),
            crate::domain::bundle::RevShareRefusal::BasisUnsupported => {
                // Unreachable: the basis was checked above and returned early.
                // Kept as a total match rather than a catch-all, so a new refusal
                // variant is a compile error here.
                String::from("rev-share is not authorable on this basis (D-55)")
            }
        };
        let code = match refusal {
            crate::domain::bundle::RevShareRefusal::ResidualOverTolerance { .. } => {
                RESIDUAL_OVER_TOLERANCE
            }
            crate::domain::bundle::RevShareRefusal::Unbalanced { .. } => REVSHARE_UNBALANCED,
            crate::domain::bundle::RevShareRefusal::BasisUnsupported => REVSHARE_BASIS_UNSUPPORTED,
        };
        report.violate(code, subject, detail);
    }
}

#[cfg(test)]
#[path = "bundle_rules_tests.rs"]
mod bundle_rules_tests;
