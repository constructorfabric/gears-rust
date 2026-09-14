//! `TaxDisplayValidator` — `inst-td-policy`, `inst-td-basis-uniform` and
//! `inst-td-gagate` of `design/04-currency-tax.md` §3, plus §4's sellability
//! state.
//!
//! §1.7 names it as *"Registered rules: `taxInclusive`/`taxCategory`
//! completeness under the tenant tax-display policy (C4) + the GA gate (C3)"*.
//! All three run over a `PlanShape` in the Foundation pipeline, because every one
//! of them is a property of a plan's **candidate row set** judged against facts
//! the caller resolved.
//!
//! # The two arms of `inst-td-policy` are not one switch, and D-154 is why
//!
//! §3 step 2 reads as a single sentence — two incomplete-basis cases "both
//! governed by the tenant tax-display policy" — and D-154 splits it in as many
//! words:
//!
//! * **`ratePresent = false` under `taxInclusive = true`** stays governed by the
//!   policy. The missing fact is a *rate*, nobody in this gear owns it, and Tax
//!   Engine is pre-GA — so a tenant may legitimately choose to warn.
//! * **an absent effective category** fails **whatever the policy says**.
//!   `taxCategory` is a pinned D-48 v1 descriptor element whose absence S2
//!   `inst-ds-required` and PRD `fr-billing-descriptors` both make a
//!   publish-blocking MUST, *"and a per-tenant display policy may not publish
//!   past a pinned contract element"*.
//!
//! Reading them as one switch is the defect D-154 was written to close: a tenant
//! on `warn` would publish a row whose category no consumer can resolve, and
//! Billing would receive a descriptor set missing one of its five.
//!
//! # The effective category, and why absence has two sources
//!
//! `inst-td-policy` evaluates `coalesce(row.tax_category_ref,
//! readiness.taxCategory)`. Both halves can be absent and they are different
//! facts:
//!
//! * the **region is undeclared** — [`RegionTaxReadiness`] answers `None`, and C4
//!   fails closed on it outright (*"an unknown region fails closed"*);
//! * the region is declared and states **no default**, and the row states none
//!   either — the effective category resolves to nothing.
//!
//! Both block, but only the second is the `TAX_BASIS_INCOMPLETE` D-154 makes
//! unconditional. The first is a region that should not be on a row at all, which
//! `inst-tx-region` refuses first and more precisely — so this module reports it
//! as the category failure it also is, rather than duplicating that rule's
//! refusal.
//!
//! # `inst-td-basis-uniform`'s row set is scoped, and the scoping is load-bearing
//!
//! D-110 requires one `tax_inclusive` per `(currency, region)`, and **D-132
//! excludes `existing_grandfathered` generations**: they are immutable in
//! content, MUST NOT be superseded and never leave `published`, so an unscoped
//! rule made one cutover freeze a market's display basis **permanently** — every
//! later publish failing on a divergent row nobody can fix. Their subscribers
//! read the basis from their own frozen snapshot, so the invoice-coherence
//! argument is unaffected.
//!
//! Every sibling row-set rule of the design set carves them out the same way
//! (`inst-bc-coverage`, `inst-sg-conjunction`, `inst-mp-grandfathered`,
//! `inst-cl-resets`), which is what makes this a set-wide convention rather than
//! a special case.

use std::collections::{BTreeMap, BTreeSet};

use toolkit_macros::domain_model;

use crate::domain::money::CurrencyCode;
use crate::domain::plan_shape::PlanShape;
use crate::domain::price_record::PriceRecord;
use crate::domain::scope_key::{PriceEligibility, Region};
use crate::domain::validation::{ValidationReport, ValidationRule};

/// A publishing row whose tax display basis is incomplete (§5, 422).
///
/// Two conditions raise it, and D-154 makes them behave differently under the
/// tenant policy — see the module doc. The message says which, because the
/// remedies are different: one is "declare a rate", the other "set a category".
pub const TAX_BASIS_INCOMPLETE: &str = "TAX_BASIS_INCOMPLETE";

/// Rows of one plan on one `(currency, region)` disagreeing on `tax_inclusive`
/// (§5, 422, D-110 — the refusal names the divergent rows).
pub const TAX_BASIS_MIXED_MARKET: &str = "TAX_BASIS_MIXED_MARKET";

// ---------------------------------------------------------------------------
// C4's input port.
// ---------------------------------------------------------------------------

/// `(tenant, region) -> { taxCategory, ratePresent }` — C4's readiness lookup, as
/// the caller resolved it.
///
/// **Fail-closed on absence**, which is C4 verbatim: a region with no entry here
/// is unknown, and unknown fails. The MVP provider is the tenant-declared
/// `tax_category` / `tax_rate_present` columns on `pricing_region_taxonomy`
/// (D-01); post-GA it becomes Tax Engine-backed and the declared markers are
/// reconciled against it.
///
/// Handed in rather than looked up, for `RoundingPolicyResolved`'s reason: the
/// rule set runs twice on one publish and a rule that fetched its own inputs
/// could answer differently in the two runs for no authored reason.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegionTaxReadiness {
    /// Per declared region: its default category, and whether a rate is declared.
    by_region: BTreeMap<String, RegionReadiness>,
}

/// One region's two declared markers.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegionReadiness {
    /// The region's default tax category. `None` is *undeclared*, not empty.
    pub tax_category: Option<String>,
    /// Tenant-declared "a tax rate is configured here".
    pub tax_rate_present: bool,
}

impl RegionTaxReadiness {
    /// Build from the resolved per-region markers.
    #[must_use]
    pub fn new(by_region: BTreeMap<String, RegionReadiness>) -> Self {
        Self { by_region }
    }

    /// The empty lookup — every region unknown, so every region fails closed.
    ///
    /// A `const fn` and not `Default::default`, because `PublishRuleParams::new`
    /// is `const` and must be able to seed the fail-closed state without a
    /// caller. `BTreeMap::new` is const; `Default` is not.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            by_region: BTreeMap::new(),
        }
    }

    /// One region's readiness by its stored string.
    ///
    /// The storage layer holds a region as a column, not a [`Region`], and
    /// wrapping one only to look it up would be a fallible conversion on a path
    /// that has nothing to do if it fails.
    #[must_use]
    pub fn of_str(&self, region: &str) -> Option<&RegionReadiness> {
        self.by_region.get(region)
    }

    /// One region's readiness, or `None` for a region nobody declared.
    #[must_use]
    pub fn of(&self, region: &Region) -> Option<&RegionReadiness> {
        self.by_region.get(region.as_str())
    }
}

/// The tenant's enforcement mode over `inst-td-policy`'s **rate** arm (C4).
#[domain_model]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaxDisplayPolicy {
    /// The ratified default: an incomplete basis blocks the publish.
    #[default]
    FailClosed,
    /// The tenant has explicitly accepted the risk on the **rate** arm. It has no
    /// effect on the category arm, which D-154 makes unconditional.
    Warn,
}

impl TaxDisplayPolicy {
    /// Both modes.
    pub const ALL: &'static [Self] = &[Self::FailClosed, Self::Warn];

    /// The stored token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FailClosed => "fail_closed",
            Self::Warn => "warn",
        }
    }

    /// Parse a stored token.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.as_str() == token)
    }
}

// ---------------------------------------------------------------------------
// The effective category (D-154).
// ---------------------------------------------------------------------------

/// `coalesce(row.tax_category_ref, readiness.taxCategory)` — the value D-154
/// makes normative and freezes into the read model.
///
/// **The authored column is untouched and stays the row's source of truth**
/// (D-110); what this resolves is what a consumer sees, so that Billing never
/// re-derives the fallback against a region taxonomy that is tenant-declared,
/// mutable and re-declarable at any time.
#[must_use]
pub fn effective_category(row: &PriceRecord, readiness: &RegionTaxReadiness) -> Option<String> {
    row.tax_category_ref.clone().or_else(|| {
        readiness
            .of(row.scope_key.region())
            .and_then(|r| r.tax_category.clone())
    })
}

/// Has the Tax Engine reached GA?
///
/// **A launch constant, platform-wide**, and deliberately not a column. C3 makes
/// the gate a property of the *engine's* status — "MVP sells tax-exclusive …
/// until Tax Engine GA (ETA ~8 months)" — so a per-tenant carrier would let one
/// tenant declare itself post-GA while the engine that has to compute the tax
/// still does not exist. That is `EVALUATION_POLICY_GENERATION`'s argument one
/// plane over: a property of the gear, not of the artifact.
///
/// It flips **in code** when the engine ships, and `inst-td-clear` is what makes
/// that safe: clearing `not_sellable_ga` is a re-publish through the pipeline
/// with approval, never a silent flag flip, so a constant changing under a frozen
/// `CatalogVersion` changes nothing already published.
pub const TAX_ENGINE_GA: bool = false;

/// §4's per-row sellability state: is this row gated pre-Tax-Engine-GA?
///
/// `inst-td-gagate`: a `taxInclusive = true` row *"MAY be authored and previewed
/// but publishes with the read-model flag `not_sellable_ga`"* — **per row, hence
/// per `(currency, region)` market**, never per plan. A plan selling
/// tax-exclusive in US and tax-inclusive in EU is gated only on its EU markets.
#[must_use]
pub const fn is_not_sellable_ga(row: &PriceRecord, tax_engine_ga: bool) -> bool {
    row.tax_inclusive && !tax_engine_ga
}

/// `inst-td-policy` (C4, D-154): the two incomplete-basis arms.
#[domain_model]
#[derive(Clone, Debug)]
pub struct TaxBasisComplete {
    /// The tenant's mode, over the **rate** arm only.
    pub policy: TaxDisplayPolicy,
    /// C4's readiness lookup.
    pub readiness: RegionTaxReadiness,
}

impl ValidationRule<PlanShape> for TaxBasisComplete {
    fn name(&self) -> &'static str {
        "inst-td-policy"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        for record in &subject.rows {
            let region = record.scope_key.region();
            let readiness = self.readiness.of(region);

            // Arm 1 — the **category**. Unconditional (D-154): a per-tenant
            // display policy may not publish past a pinned D-48 v1 element.
            if effective_category(record, &self.readiness).is_none() {
                report.violate(
                    TAX_BASIS_INCOMPLETE,
                    record.price_id.to_string(),
                    format!(
                        "row {} in region `{region}` resolves no tax category: it states none and \
                         the region declares no default, so `coalesce(row.tax_category_ref, \
                         readiness.taxCategory)` is empty. `taxCategory` is a pinned D-48 v1 \
                         descriptor element, so this blocks publish whatever the tenant \
                         tax-display policy says (D-154) — set the row's category, or declare a \
                         default on the region",
                        record.price_id
                    ),
                );
            }

            // Arm 2 — the **rate**, on a tax-inclusive row. Governed by the
            // policy, because the missing fact is one nobody in this gear owns
            // and Tax Engine is pre-GA.
            if !record.tax_inclusive {
                continue;
            }
            let rate_present = readiness.is_some_and(|r| r.tax_rate_present);
            if rate_present {
                continue;
            }
            let detail = format!(
                "row {} is tax-inclusive in region `{region}`, which declares no tax rate \
                 (`tax_rate_present = false`, or the region is not declared at all — either way \
                 readiness fails closed, C4). A tax-inclusive amount cannot be decomposed \
                 without a rate",
                record.price_id
            );
            match self.policy {
                TaxDisplayPolicy::FailClosed => {
                    report.violate(TAX_BASIS_INCOMPLETE, record.price_id.to_string(), detail);
                }
                TaxDisplayPolicy::Warn => {
                    report.warn(TAX_BASIS_INCOMPLETE, record.price_id.to_string(), detail);
                }
            }
        }
    }
}

/// The `(currency, region)` pair a row sells on — D-110's "market".
///
/// Named because both axes are load-bearing and a bare tuple invites reading it
/// as "the region": `EUR/eu` and `USD/eu` are two markets and may legitimately
/// carry different display bases.
type Market = (CurrencyCode, Region);

/// The row ids on each side of a market's basis, keyed by the basis itself.
///
/// A map rather than two vectors so the refusal can render every side it found,
/// which is what §5's "naming the divergent rows" asks for.
type BasesInMarket = BTreeMap<bool, BTreeSet<String>>;

/// `inst-td-basis-uniform` (D-110, scoped by D-132): one display basis per
/// market.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct MarketBasisUniform;

impl ValidationRule<PlanShape> for MarketBasisUniform {
    fn name(&self) -> &'static str {
        "inst-td-basis-uniform"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        // Grouped by market, and **`existing_grandfathered` rows are excluded**
        // before the grouping rather than filtered out of a finding afterwards:
        // an immutable generation must not even contribute a basis to compare
        // against, or it would still decide the market's verdict (D-132).
        let mut markets: BTreeMap<Market, BasesInMarket> = BTreeMap::new();
        for record in &subject.rows {
            if record.scope_key.price_eligibility() == PriceEligibility::ExistingGrandfathered {
                continue;
            }
            markets
                .entry((
                    record.scope_key.currency().clone(),
                    record.scope_key.region().clone(),
                ))
                .or_default()
                .entry(record.tax_inclusive)
                .or_default()
                .insert(record.price_id.to_string());
        }

        for ((currency, region), by_basis) in markets {
            if by_basis.len() < 2 {
                continue;
            }
            // §5 requires the refusal to **name the divergent rows**, so both
            // sides are rendered: an operator told only "this market is mixed"
            // has to go and find which rows disagree.
            let sides: Vec<String> = by_basis
                .iter()
                .map(|(inclusive, ids)| {
                    format!(
                        "tax_inclusive={inclusive}: {}",
                        ids.iter().cloned().collect::<Vec<_>>().join(", ")
                    )
                })
                .collect();
            report.violate(
                TAX_BASIS_MIXED_MARKET,
                format!("{}/{region}", currency.as_str()),
                format!(
                    "rows of this plan on market {}/{region} disagree on tax_inclusive — {}. An \
                     invoice is one document and `tax_inclusive` is a display basis, so a \
                     tax-inclusive recurring line beside a tax-exclusive usage line is not \
                     renderable coherently (D-110). Grandfathered generations are excluded from \
                     this set (D-132)",
                    currency.as_str(),
                    sides.join(" | ")
                ),
            );
        }
    }
}

#[cfg(test)]
#[path = "tax_display_tests.rs"]
mod tax_display_tests;
