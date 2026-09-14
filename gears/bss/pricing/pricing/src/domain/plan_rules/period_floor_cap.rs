//! The plan-level period floor and cap
//! (`cpt-cf-bss-pricing-algo-period-floor-cap`, **D-319**).
//!
//! Two rules over a [`PlanShape`]'s [`period_floor_caps`] set: the bound is
//! authored on a market the plan actually sells, and each bound is a usable
//! amount.
//!
//! [`period_floor_caps`]: PlanShape::period_floor_caps
//!
//! # What this gear does with the value: nothing
//!
//! The floor and the cap are **period-level phases over the aggregated total**,
//! applied after per-line emission. Rating resolves them from the pinned
//! snapshot and emits a `PeriodFloorCapObligation`; **Billing** executes
//! `max(total, floor)` / `min(total, cap)` and rounds. This gear authors,
//! validates and freezes — the deferred half of PRD §17.8 — and adds no
//! evaluation machinery.
//!
//! # Why the amount rule exists when the column already carries the `CHECK`
//!
//! All four amount faults — a non-positive floor, a non-positive cap, a floor
//! above its cap, and an entry authoring neither — are `CHECK`s in
//! `pricing_plan_period_floor_cap`, so the write path
//! refuses them and this rule can only fire on a row that reached the table
//! another way. It is registered anyway, and **D-150** is the precedent and the
//! argument: the add-on quantity bounds were a `CHECK` with no code, so an
//! author met an unsatisfiable selection as a driver error rather than as a
//! report line naming the offending bound. The constraint is the guarantee and
//! the rule is the explanatory path — the arrangement D-148 describes.
//!
//! **D-150's arrangement needs the authoring door, not just the `CHECK`s.** The
//! add-on precedent it cites has one — `require_authorable_addon_bounds` runs
//! `inst-cmp-addonqty` over the submitted rules — and without the matching door here
//! the `CHECK`s are not the backstop of an explanatory path but the only thing
//! reading the field: a `0` floor reaches the caller as `500 … please retry later`,
//! and since every publish subject is assembled from storage, all four arms below
//! are dead and `PERIOD_FLOOR_CAP_AMOUNT_INVALID` is a declared code with no raiser.
//! The rule runs at the `period_floor_caps` facet's parse
//! (`api::rest::plans::require_authorable_period_bounds`) as well as at publish, so
//! the rule is the path an author meets and the `CHECK` is what no author reaches.
//!
//! # Why the market rule cannot be a `CHECK`
//!
//! "This bound's `(currency, region)` is one the plan sells" is a property of
//! the plan's **row set**, not of the bound row: the market set is the union
//! over `pricing_price`'s scope keys, which no constraint on this table can
//! see. It is `PERIOD_FLOOR_CAP_MARKET_UNSOLD` at publish, in the same family
//! as `BASE_MARKET_INCOMPLETE` and `USAGE_MARKET_INCOMPLETE` — and pointing the
//! other way. Those two ask whether every sold market carries the rows it owes;
//! this one asks whether an authored bound has a market to apply in. A floor
//! authored on `(USD, us)` by a plan selling `(USD, ca)` is not a smaller floor,
//! it is **no floor at all**, frozen for the seven-year horizon into an
//! immutable snapshot, and the author who wrote it believes their plan has a
//! minimum.
//!
//! The converse is deliberately **not** a rule: a sold market with no bound is
//! the ordinary plan, and requiring one would make every plan in the catalogue
//! author a minimum.

use toolkit_macros::domain_model;

use crate::domain::money::MinorAmount;
use crate::domain::plan_rules::{PERIOD_FLOOR_CAP_AMOUNT_INVALID, PERIOD_FLOOR_CAP_MARKET_UNSOLD};
use crate::domain::plan_shape::PlanShape;
use crate::domain::validation::{ValidationReport, ValidationRule};

/// `inst-pfc-market` — every authored bound names a market the plan sells.
#[domain_model]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeriodFloorCapMarketSold;

impl ValidationRule<PlanShape> for PeriodFloorCapMarketSold {
    fn name(&self) -> &'static str {
        "inst-pfc-market"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        // Derived once, from `PlanShape::markets`, so this rule cannot disagree
        // with D-84's completeness rules about which markets a plan is in.
        let sold = subject.markets();
        // Every offending bound is reported, never the first: an author
        // remediates a plan in one pass, and a plan that mis-typed two markets
        // would otherwise take two publishes to learn about both.
        for bound in &subject.period_floor_caps {
            if sold.contains(&bound.market()) {
                continue;
            }
            report.violate(
                PERIOD_FLOOR_CAP_MARKET_UNSOLD,
                subject.subject(),
                format!(
                    "a period floor/cap is authored on ({currency}, {region}), a market this \
                     plan prices nothing in: the bound is compared against a period total that \
                     no row of this plan can contribute to, so it would freeze into the \
                     snapshot as a minimum nothing can reach",
                    currency = bound.currency,
                    region = bound.region.as_str(),
                ),
            );
        }
    }
}

/// `inst-pfc-amount` — each authored bound is a usable amount.
///
/// Four faults, one code, the offending bound named — `ADDON_QTY_RANGE_INVALID`'s
/// arrangement (D-150), and for its reason: the operator's next action is the
/// same shape in every case, which is to author a bound that admits a bill.
///
/// The empty entry `continue`s rather than falling through to the other three:
/// a bound authoring nothing has no amount for them to judge, and reporting it
/// twice would read as two faults to whoever has to fix the one.
#[domain_model]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeriodFloorCapAmounts;

impl ValidationRule<PlanShape> for PeriodFloorCapAmounts {
    fn name(&self) -> &'static str {
        "inst-pfc-amount"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        for bound in &subject.period_floor_caps {
            let market = format!("({}, {})", bound.currency, bound.region.as_str());
            let mut complain = |detail: String| {
                report.violate(PERIOD_FLOOR_CAP_AMOUNT_INVALID, subject.subject(), detail);
            };
            if bound.floor_minor.is_none() && bound.cap_minor.is_none() {
                complain(format!(
                    "the period bound on {market} authors neither a floor nor a cap: absence is \
                     how a plan says it has no minimum, so a bound that says nothing is a row \
                     with no reading"
                ));
                continue;
            }
            if bound.floor_minor.is_some_and(is_zero) {
                complain(format!(
                    "the period floor on {market} is 0: every line is already held at or above \
                     zero before the floor applies, so `max(total, 0)` changes nothing — \
                     absence is the only spelling of \"no minimum\""
                ));
            }
            if bound.cap_minor.is_some_and(is_zero) {
                complain(format!(
                    "the period cap on {market} is 0: a plan that may never charge anything is \
                     not a capped plan, it is an unsellable one"
                ));
            }
            if let (Some(floor), Some(cap)) = (bound.floor_minor, bound.cap_minor)
                && floor > cap
            {
                complain(format!(
                    "the period floor on {market} is {floor} and its cap is {cap}: the bound \
                     describes a period total that must be both at least {floor} and at most \
                     {cap}, which no invoice satisfies"
                ));
            }
        }
    }
}

/// Whether an authored amount is the zero this rule refuses.
///
/// [`MinorAmount`] admits zero on purpose — a free trial phase and a
/// zero-rated usage row are both authored as `0` — so the strictness is
/// D-319's, stated here rather than pushed into the money type where it would
/// change what a price row may say.
fn is_zero(amount: MinorAmount) -> bool {
    amount.get() == 0
}

#[cfg(test)]
#[path = "period_floor_cap_tests.rs"]
mod period_floor_cap_tests;
