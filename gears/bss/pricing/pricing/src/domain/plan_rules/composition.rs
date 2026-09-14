//! Plan composition rules (`cpt-cf-bss-pricing-algo-composition`).
//!
//! Rules over a [`PlanShape`]: the plan's tier is declared, its priced metered
//! lines are unambiguous, and the properties D-16 and D-150 ask of the
//! plan-authored add-on graph hold.
//! [`plan_rules::plan_shape_rules`](super::plan_shape_rules) registers them and is
//! the roster. Each is the half of its instruction that
//! needs nothing outside this gear; the halves that read the product / SKU
//! registry are absent, and the last section of this doc says which and why.
//!
//! ## Meter injectivity is per line per slice, and D-103 says why
//!
//! `inst-cmp-injective` is **one priced line per `(meter, dimensionKey)` per
//! scope-key slice**, not "each usage plan revision maps exactly one
//! `meteringUnit`". Three rules of its own slice contradict the stronger reading —
//! D-84's per-market completeness ranges over "every `(meter, dimensionKey)` line
//! the plan prices", the slice's own D-84 integration case exercises a plan pricing
//! two meters, and D-43's grant `applicability` scopes to a **set** of published
//! meters — and nothing enforces it: the partial `UNIQUE` that does the enforcing
//! carries `meter` **and** `dimension_key`, so it implements the per-line reading.
//! A plan pricing cloudlets, storage and egress is one plan rather than three.
//!
//! The slice is `(currency, region, priceOverlay, phase, priceEligibility,
//! cohort)`. Those six axes legitimately multiply rows, and `cohort` is the one
//! that carries the weight: it is what lets a second cutover add another
//! grandfathered generation of the same usage line without the new generation
//! reading as a duplicate of the old (ADR-0002). Only a duplicate **inside** one
//! slice is the ambiguity that fails publish, because that is the only shape
//! where nothing decides which row prices the usage.
//!
//! ### It selects rows by the presence of a meter, not by `chargeKind`
//!
//! §6's index is `(tenant_id, plan_id, currency, region, price_overlay, phase,
//! price_eligibility, cohort, meter, dimension_key)`. `charge_kind` is not one
//! of its columns, and what keeps non-metered rows out of it is that their
//! `meter` is NULL and NULLs never collide. This rule selects on the same thing
//! — a row carrying a meter — rather than on `chargeKind`, and deliberately not
//! through [`PlanShape::usage_rows`]. Selecting on `chargeKind` would put a
//! mis-filed metered row (a meter on a row whose charge component is not
//! `usage`) outside the pipeline's reach while leaving it inside the index's,
//! and the author would then meet the same fault as a driver error at commit
//! rather than as a line in the report. Restating the index in the pipeline is
//! the point of the rule; restating it over a different row set would be a
//! second, disagreeing answer.
//!
//! ## Add-on edges are plan-authored (D-16)
//!
//! The dependency and conflict edges are **not** registry compatibility
//! metadata. Each rule row may declare `depends_on` / `conflicts_with` sets
//! naming *other add-ons of the same plan's set*, and the three rules here are
//! the three ways that set can be incoherent on its own terms: an edge pointing
//! out of the set (or at itself), a dependency cycle, and a conflicting pair
//! both of whose sides are `required`.
//!
//! **A conflict is not a fault.** A conflicting pair with at least one optional
//! side publishes as a selection-time constraint and is exactly what the field
//! exists to express; only a pair that is *both* required is a fault, because
//! then no admissible selection exists at all and the plan is publishable and
//! unbuyable. A rule that failed every conflict pair would forbid the feature.
//!
//! ## What is deliberately absent
//!
//! The precedent is [`crate::domain::rules`]'s own absence section — *a rule
//! that always passes is indistinguishable from a rule that holds* — and
//! [`crate::domain::plan_rules`] carries the code-level table. What this
//! algorithm loses to it:
//!
//! - **`inst-cmp-plantier`, second half** (`PLANTIER_DIVERGENT`) and
//!   **`inst-cmp-tier-drift`**: both need the parent SKU's `PlanTier` from the
//!   registry read model, which this gear has no client for
//!   ([`crate::domain::ports`] holds the `CatalogVersion` registry and nothing
//!   else). The `plan_tier_override` audit has nothing to audit until the
//!   equality it overrides can be computed.
//! - **`SKU_NOT_PUBLISHED`**: the parent SKU's publication state, same registry.
//! - **`inst-cmp-usagetype`** (`METER_USAGE_TYPE_UNBOUND`,
//!   `METER_DIMENSION_UNDECLARED`, and its drift arm): the metering-unit
//!   declaration's `usageTypeRef` and the `UsageType`'s declared
//!   `metadata_fields` keys, same registry.
//! - **The registry half of `ADDON_INCOMPATIBLE`**: whether the add-on SKUs are
//!   published and compatible with the base SKU. The plan-authored half is here.
//! - **`inst-cmp-override-home`** (`ADDON_OVERRIDE_UNRESOLVED`): needs two
//!   things at once. The add-on SKU's own plans, to resolve `price_override_ref`
//!   to a published `priceId` authored there — a registry join. And
//!   `PriceWindow` coverage, to check that a covering **member** key exists for
//!   every `(currency, region)` the base plan sells: D-97 as amended by D-116
//!   binds the key *family* modulo the market axes, with `(currency, region)`
//!   free. Requiring one canonical key to "cover every market" instead would be
//!   unsatisfiable rather than merely unimplemented, since a single canonical key
//!   fixes one market.
//!
//! One more absence has no registry in it. `inst-cmp-addons` also states that
//! **a required add-on has `maxQty >= 1`**, and §5 names no code for it. It is
//! enforced physically — §6 puts a `max_qty >= 1 WHERE required` CHECK on
//! `pricing_plan_addon_rule` — but reporting it from the pipeline would mean
//! either minting a code the design set has not, or stretching
//! `ADDON_INCOMPATIBLE` to cover a quantity bound it says nothing about, and a
//! code reported for something §5 does not say it reports is a code no consumer
//! can act on.

use std::collections::{BTreeMap, BTreeSet};

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::money::CurrencyCode;
use crate::domain::plan_rules::{
    ADDON_CYCLE, ADDON_INCOMPATIBLE, ADDON_QTY_RANGE_INVALID, METER_AMBIGUOUS, PLAN_NAME_INVALID,
    PLANTIER_MISSING,
};
use crate::domain::plan_shape::{AddonRule, PlanShape};
use crate::domain::price_record::PriceRecord;
use crate::domain::scope_key::{Cohort, PhaseId, PriceEligibility, PriceOverlay, Region};
use crate::domain::validation::{ValidationReport, ValidationRule};

// ---------------------------------------------------------------------------
// D-318 - the plan name
// ---------------------------------------------------------------------------

/// The plan's name, if it carries one, is a name (D-318).
///
/// Two refusals and no third. **Blank** — empty or whitespace-only — because
/// `NULL` already means "unnamed" and a second spelling of one state is a defect
/// this gear has paid for before: a list would then have to treat `""` and
/// absent alike everywhere, and the first surface that forgot would show a plan
/// with a name made of nothing. **Over the bound**, because a name is rendered
/// in a table cell and a nav breadcrumb, and there is no length at which a
/// caller's intent is served by 8 KiB of text in a column meant for a label.
///
/// [`Stage::Write`](crate::domain::validation::Stage) by D-312's criterion:
/// every operand of the fault is in the request the author just sent, and none
/// of it is frozen by anything the publish resolves. An author cannot be part
/// way to a valid name — the intermediate state a write-stage refusal would
/// wrongly block does not exist here, unlike a half-authored band ladder.
///
/// **No uniqueness rule, deliberately.** Identity is `plan_id`; a name is a
/// label, and two plans may legitimately share one (a tenant running the same
/// offer in two markets). Enforcing it would be a cross-row check at publish
/// whose cost is a scan per publish, buying a convention an operator can hold
/// without the gear. Stated because an absence a reader cannot tell from an
/// oversight is one they will re-litigate.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanNameWellFormed;

/// The longest name any surface undertakes to render.
///
/// 120 characters. The longest name in the Pricing Studio prototype's own
/// catalog is 34 ("Chainstack — Platform (restricted)"), so this is generous by
/// more than three times, and it is counted in **characters rather than bytes**
/// so that a name in a non-Latin script is not silently allowed a third of the
/// room.
pub const PLAN_NAME_MAX_CHARS: usize = 120;

/// The fault in a plan name, or `None` if it is one.
///
/// Extracted so the **route** and the **rule** judge by the same code. They must
/// both judge: the rule is what the publish pipeline and any other assembler of
/// a shape run, and the route is the only thing between an author and a column
/// holding `''` — the plan write path runs no rule pipeline at all, which is
/// what a REST probe found after this rule's doc had already claimed the write
/// stage for it.
#[must_use]
pub fn plan_name_fault(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return Some(
            "planName is blank: a plan with no name is spelled by omitting the field, and \
             storing an empty one would give the unnamed state two spellings"
                .to_owned(),
        );
    }
    let chars = name.chars().count();
    if chars > PLAN_NAME_MAX_CHARS {
        return Some(format!(
            "planName is {chars} characters, above the {PLAN_NAME_MAX_CHARS} a surface \
             undertakes to render"
        ));
    }
    None
}

impl ValidationRule<PlanShape> for PlanNameWellFormed {
    fn name(&self) -> &'static str {
        "inst-cmp-planname"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        let Some(name) = subject.plan_name.as_deref() else {
            return;
        };
        if let Some(detail) = plan_name_fault(name) {
            report.violate_at_write(PLAN_NAME_INVALID, subject.subject(), detail);
        }
    }
}

// ---------------------------------------------------------------------------
// PlanTier (inst-cmp-plantier)
// ---------------------------------------------------------------------------

/// `inst-cmp-plantier`, first half — the tier is declared before publish.
///
/// It is optional while the plan is a draft and required at publish, because
/// the effective tier is a published field: it lands in the read model and
/// consumers resolve it from there. An undeclared tier at publish would freeze
/// an absence into a `CatalogVersion` nothing can revise in place.
///
/// The equality-to-SKU half (`PLANTIER_DIVERGENT`) and the audited-override
/// arm are registry work and are absent; see the module doc. That is the reason
/// this rule also refuses a **blank** tier: a whitespace-only string is an
/// absence wearing a value's shape, and the equality check that would otherwise
/// have caught it is the one thing this gear cannot run.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanTierDeclared;

impl ValidationRule<PlanShape> for PlanTierDeclared {
    fn name(&self) -> &'static str {
        "inst-cmp-plantier"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        let declared = subject
            .plan_tier
            .as_deref()
            .is_some_and(|tier| !tier.trim().is_empty());
        if declared {
            return;
        }
        report.violate(
            PLANTIER_MISSING,
            subject.subject(),
            "planTier is optional on a draft and required at publish, and a blank value is not \
             a declaration: the effective tier is published into the read model, so an absence \
             here freezes into a catalog version rather than staying editable",
        );
    }
}

// ---------------------------------------------------------------------------
// Meter injectivity (inst-cmp-injective, D-103)
// ---------------------------------------------------------------------------

/// `inst-cmp-injective` — one priced line per `(meter, dimensionKey)` per
/// scope-key slice (D-103).
///
/// This is the pipeline's restatement of the partial `UNIQUE` §6 puts on
/// current published usage rows, and the duplication is deliberate: the index
/// refuses the write, so without the rule the author would meet a duplicate
/// line as a driver error inside the publish commit instead of as a named
/// finding in the aggregate report. See the module doc for the row set it ranges
/// over and why it is not `chargeKind`.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct MeterInjectivity;

impl ValidationRule<PlanShape> for MeterInjectivity {
    fn name(&self) -> &'static str {
        "inst-cmp-injective"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        let mut priced: BTreeMap<MeteredLine, Uuid> = BTreeMap::new();
        for record in &subject.rows {
            let Some(line) = metered_line(record) else {
                continue;
            };
            let duplicate = record.price_id;
            if let Some(first) = priced.get(&line).copied() {
                report.violate(
                    METER_AMBIGUOUS,
                    line.subject(),
                    format!(
                        "price {first} and price {duplicate} both price this \
                         (meter, dimensionKey) line inside one scope-key slice, and nothing \
                         decides which of them the metered usage resolves to. currency, region, \
                         priceOverlay, phase, priceEligibility and cohort legitimately multiply \
                         rows, and a plan may price several metering units (D-103) -- only a \
                         duplicate line within one slice fails publish"
                    ),
                );
                continue;
            }
            priced.insert(line, duplicate);
        }
    }
}

/// One priced metered line, keyed by the scope-key slice injectivity holds
/// within.
///
/// `charge_kind` is absent by construction rather than by omission: it is not a
/// column of §6's index either, and including it here would split one slice into
/// four and let a mis-filed metered row through the pipeline into the index.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MeteredLine {
    currency: CurrencyCode,
    region: Region,
    price_overlay: PriceOverlay,
    phase: PhaseId,
    price_eligibility: PriceEligibility,
    cohort: Cohort,
    meter: String,
    dimension_key: String,
}

impl MeteredLine {
    /// How an ambiguity locates itself: the line, then the slice it duplicates
    /// within.
    ///
    /// Both halves are needed. The line alone would report a plan pricing one
    /// meter in six markets as six copies of the same subject, and the slice
    /// alone would not say which of the slice's lines is doubled. An
    /// undimensioned row renders an empty `dimensionKey`, which is the
    /// empty-tuple sentinel the index collides on rather than a missing value.
    fn subject(&self) -> String {
        format!(
            "{}/{}@{}|{}|{}|{}|{}|{}",
            self.meter,
            self.dimension_key,
            self.currency,
            self.region,
            self.price_overlay,
            self.phase,
            self.price_eligibility,
            self.cohort
        )
    }
}

/// The line a row prices, or `None` when the row prices no meter.
fn metered_line(record: &PriceRecord) -> Option<MeteredLine> {
    let meter = record.row.meter.clone()?;
    let key = &record.scope_key;
    Some(MeteredLine {
        currency: key.currency().clone(),
        region: key.region().clone(),
        price_overlay: key.price_overlay(),
        phase: key.phase(),
        price_eligibility: key.price_eligibility(),
        cohort: key.cohort(),
        meter,
        dimension_key: record.row.dimension_key.clone(),
    })
}

// ---------------------------------------------------------------------------
// Add-on composition (inst-cmp-addons, D-16)
//
// One instruction states four separable properties of the add-on set, so the
// three rules beside the membership one carry a suffix - the arrangement
// `phase_graph` uses for `inst-ph-graph`. What the name reaches is
// `ValidationPipeline::rule_names`, the registered roster: a `Violation` carries
// a code and not a rule name, so this is what a census of the pipeline can see
// and nothing else. Four rules answering to one name leave that roster unable to
// say which of the four is registered, which is the only question it answers. A
// suffix is not an invented id: it names the instruction and then the property,
// so the roster distinguishes them without claiming four instructions exist.
// ---------------------------------------------------------------------------

/// `inst-cmp-addons` — every authored edge names another add-on of the same
/// plan's set.
///
/// D-16 scopes both edge sets to the plan's own add-on rules, and an edge out of
/// that set is unresolvable at selection time: the target carries no rule this
/// revision holds, so nothing states its quantity bounds, its `required` flag or
/// its own edges. A self-edge is out of the set in the sense that matters — the
/// design says *other* add-ons — and it is a dependency nothing can satisfy or a
/// conflict with itself, so it is reported here too.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct AddonEdgeMembership;

impl ValidationRule<PlanShape> for AddonEdgeMembership {
    fn name(&self) -> &'static str {
        "inst-cmp-addons"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        let members = addon_ids(&subject.addon_rules);
        for rule in &subject.addon_rules {
            for &target in &rule.depends_on {
                check_edge(rule.addon_sku_id, target, "depends_on", &members, report);
            }
            for &target in &rule.conflicts_with {
                check_edge(
                    rule.addon_sku_id,
                    target,
                    "conflicts_with",
                    &members,
                    report,
                );
            }
        }
    }
}

/// `inst-cmp-addons` — the `depends_on` graph is acyclic.
///
/// A cycle makes the add-on set unsatisfiable rather than merely odd: every
/// member of the cycle requires another member, so no subset of the plan's
/// add-ons closes under `depends_on` and the plan publishes as something nobody
/// can buy a valid configuration of.
///
/// **Self-edges are not reported here.** They are cycles of length one, and
/// [`AddonEdgeMembership`] already refuses them; reporting one authoring mistake
/// under two codes would make it look like two, which is the reason
/// `inst-pk-fields` keeps the `package` x bands case to itself.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct AddonDependencyAcyclic;

impl ValidationRule<PlanShape> for AddonDependencyAcyclic {
    fn name(&self) -> &'static str {
        "inst-cmp-addons/acyclic"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        for members in dependency_cycles(&dependency_edges(&subject.addon_rules)) {
            report.violate(
                ADDON_CYCLE,
                render_ids(&members),
                "these add-ons depend on one another in a closed loop over the plan-authored \
                 depends_on edges (D-16): every member requires another member, so no selection \
                 of the plan's add-ons satisfies the set",
            );
        }
    }
}

/// `inst-cmp-addons` — a conflicting pair with **both** sides `required`.
///
/// The conflict is read symmetrically whichever side authored it, matching the
/// normalized-symmetric storage §6 specifies: a conflict that bound only its
/// author would be a constraint the other side could ignore.
///
/// A pair with at least one optional side is **not** a violation. It publishes
/// as a selection-time constraint, which is the whole of what the field is for;
/// only both-required leaves no admissible selection at all.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct AddonConflictBothRequired;

impl ValidationRule<PlanShape> for AddonConflictBothRequired {
    fn name(&self) -> &'static str {
        "inst-cmp-addons/conflict"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        let required: BTreeSet<Uuid> = subject
            .addon_rules
            .iter()
            .filter(|rule| rule.required)
            .map(|rule| rule.addon_sku_id)
            .collect();
        let mut pairs: BTreeSet<(Uuid, Uuid)> = BTreeSet::new();
        for rule in &subject.addon_rules {
            let owner = rule.addon_sku_id;
            for &other in &rule.conflicts_with {
                // A target outside the set carries no `required` flag to read,
                // and a self-conflict is `AddonEdgeMembership`'s to report; both
                // fall out of the membership test below rather than needing an
                // arm of their own.
                if other == owner || !required.contains(&owner) || !required.contains(&other) {
                    continue;
                }
                pairs.insert(if owner <= other {
                    (owner, other)
                } else {
                    (other, owner)
                });
            }
        }
        for (lower, upper) in pairs {
            report.violate(
                ADDON_INCOMPATIBLE,
                format!("{lower},{upper}"),
                "these two add-ons conflict and both are marked required, so every selection \
                 either omits a required add-on or takes a conflicting pair. A conflict with at \
                 least one optional side is a selection-time constraint and publishes",
            );
        }
    }
}

/// The add-on SKU ids this plan's rule set holds — the set every edge must name
/// a member of.
fn addon_ids(rules: &[AddonRule]) -> BTreeSet<Uuid> {
    rules.iter().map(|rule| rule.addon_sku_id).collect()
}

/// Judge one authored edge against the plan's own add-on set.
fn check_edge(
    owner: Uuid,
    target: Uuid,
    edge: &'static str,
    members: &BTreeSet<Uuid>,
    report: &mut ValidationReport,
) {
    let detail = if owner == target {
        format!(
            "{edge} names the rule's own add-on: D-16 scopes these edges to *other* add-ons of \
             the same plan's set, and an edge to itself is a dependency nothing can satisfy or a \
             conflict with itself"
        )
    } else if members.contains(&target) {
        return;
    } else {
        format!(
            "{edge} names an add-on this plan's rule set does not hold: the edges are \
             plan-authored (D-16), so a target outside the set has no rule stating its quantity \
             bounds, its required flag or its own edges, and nothing resolves it at selection time"
        )
    };
    report.violate(ADDON_INCOMPATIBLE, format!("{owner}/{target}"), detail);
}

/// The `depends_on` adjacency, restricted to edges the walk can follow.
///
/// Out-of-set targets and self-edges are dropped: both are
/// [`AddonEdgeMembership`]'s findings, and an out-of-set target has no rule row
/// and therefore no outgoing edges, so it could not lie on a cycle in any case.
fn dependency_edges(rules: &[AddonRule]) -> BTreeMap<Uuid, BTreeSet<Uuid>> {
    let members = addon_ids(rules);
    let mut edges: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
    for rule in rules {
        let owner = rule.addon_sku_id;
        let followable = rule
            .depends_on
            .iter()
            .copied()
            .filter(|target| *target != owner && members.contains(target));
        edges.entry(owner).or_default().extend(followable);
    }
    edges
}

/// The closed loops of the `depends_on` graph, each as its member set.
///
/// Members, not a traversal path, and the distinction is what makes the report
/// reproducible: one loop has as many paths through it as it has members, so
/// naming a path would let two runs over the same plan report the same fault
/// with different strings depending on which node the walk happened to enter at.
/// A node that merely *depends into* a loop is not a member — it can reach the
/// loop but the loop cannot reach it — so it is named by no violation, which is
/// right, because removing it does not break the loop.
fn dependency_cycles(edges: &BTreeMap<Uuid, BTreeSet<Uuid>>) -> Vec<BTreeSet<Uuid>> {
    let reach: BTreeMap<Uuid, BTreeSet<Uuid>> = edges
        .keys()
        .map(|&node| (node, reachable_from(edges, node)))
        .collect();
    let looping: BTreeSet<Uuid> = reach
        .iter()
        .filter(|(node, targets)| targets.contains(node))
        .map(|(node, _)| *node)
        .collect();

    let mut loops: Vec<BTreeSet<Uuid>> = Vec::new();
    let mut placed: BTreeSet<Uuid> = BTreeSet::new();
    for &node in &looping {
        if placed.contains(&node) {
            continue;
        }
        let members: BTreeSet<Uuid> = looping
            .iter()
            .copied()
            .filter(|&other| reaches(&reach, node, other) && reaches(&reach, other, node))
            .collect();
        placed.extend(members.iter().copied());
        loops.push(members);
    }
    loops
}

/// Every node reachable from `start` by one or more edges.
///
/// `start` itself is a member exactly when it lies on a loop, which is the whole
/// question [`dependency_cycles`] asks — so the walk deliberately does not seed
/// the visited set with it.
fn reachable_from(edges: &BTreeMap<Uuid, BTreeSet<Uuid>>, start: Uuid) -> BTreeSet<Uuid> {
    let mut seen: BTreeSet<Uuid> = BTreeSet::new();
    let mut frontier: Vec<Uuid> = vec![start];
    while let Some(node) = frontier.pop() {
        let Some(targets) = edges.get(&node) else {
            continue;
        };
        for &target in targets {
            if seen.insert(target) {
                frontier.push(target);
            }
        }
    }
    seen
}

/// Can `from` reach `to` over the `depends_on` edges?
fn reaches(reach: &BTreeMap<Uuid, BTreeSet<Uuid>>, from: Uuid, to: Uuid) -> bool {
    reach
        .get(&from)
        .is_some_and(|targets| targets.contains(&to))
}

/// Ids in ascending order, one string.
///
/// The ordering is the reproducibility guarantee an `ADDON_CYCLE` subject
/// carries: the same plan reports the same members in the same order however the
/// rules were authored or read back.
fn render_ids(ids: &BTreeSet<Uuid>) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<String>>()
        .join(",")
}

#[cfg(test)]
#[path = "composition_tests.rs"]
mod composition_tests;

// ---------------------------------------------------------------------------
// inst-cmp-addons, the quantity bounds (D-150)
// ---------------------------------------------------------------------------

/// An add-on's quantity bounds admit at least one selection.
///
/// Three bounds, each an unsatisfiable-selection defect on one row:
///
/// - `required => maxQty >= 1` — a mandatory add-on nobody may take any of;
/// - `minQty <= maxQty` when both are set — a window with nothing in it;
/// - `stepQty > 0` when set — a step that never advances.
///
/// **All three are reported, not the first.** §9's contract is that the 422
/// enumerates every violation, and a plan with three broken bounds gets three
/// lines: an author remediates in one pass, and a report truncated at the first
/// fault sends them round the pipeline once per bound.
///
/// **The three `CHECK`s on `pricing_plan_addon_rule` stay.** The rule is the
/// explanatory path and the constraint the guarantee (D-148's read-then-index
/// arrangement), and only one of the three was ever written as a `CHECK` anyway.
/// What changes is that an author meets a report line naming the bound rather
/// than a driver error naming a constraint — and that the other two were
/// expressible at all, so an add-on nobody can select published on a plan that
/// looked complete.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct AddonQtyRange;

impl ValidationRule<PlanShape> for AddonQtyRange {
    fn name(&self) -> &'static str {
        "inst-cmp-addons/qty"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        for rule in &subject.addon_rules {
            let subject_ref = format!("{}|{}", subject.subject(), rule.addon_sku_id);
            // `is_none_or`, not `is_some_and`: the rule is `required => maxQty >= 1`
            // and an **absent** ceiling fails it exactly as a zero one does. With
            // `is_some_and` the absent case fell through the rule and landed on
            // `chk_pricing_plan_addon_rule_required_max_qty` instead, which reaches
            // the caller as a **500** naming a constraint — the one outcome this
            // rule's own doc says it exists to prevent. Found by seeding a catalog:
            // every `required: true` add-on 500ed, and none of them set a ceiling.
            if rule.required && rule.max_qty.is_none_or(|max| max < 1) {
                report.violate(
                    ADDON_QTY_RANGE_INVALID,
                    subject_ref.clone(),
                    format!(
                        "maxQty is {} on a required add-on ({}): the plan mandates an add-on and \
                         permits nobody to take any of it, so no selection satisfies the plan",
                        rule.max_qty
                            .map_or_else(|| "unset".to_owned(), |max| max.to_string()),
                        rule.addon_sku_id
                    ),
                );
            }
            if let (Some(min), Some(max)) = (rule.min_qty, rule.max_qty)
                && min > max
            {
                report.violate(
                    ADDON_QTY_RANGE_INVALID,
                    subject_ref.clone(),
                    format!(
                        "minQty {min} is above maxQty {max} on add-on {}: the selection window \
                         is empty",
                        rule.addon_sku_id
                    ),
                );
            }
            if rule.step_qty.is_some_and(|step| step == 0) {
                report.violate(
                    ADDON_QTY_RANGE_INVALID,
                    subject_ref,
                    format!(
                        "stepQty is 0 on add-on {}: a step of zero never advances, so the only \
                         selectable quantity is minQty and the bound says otherwise",
                        rule.addon_sku_id
                    ),
                );
            }
        }
    }
}
