//! Slice 10's derived (composite) meter — the two rules that need no
//! counterparty (`inst-cm-constituents`' arity half, `inst-cm-formula`'s
//! self-reference half).
//!
//! # Why these are `PlanShape` rules and not row-local ones
//!
//! A composite is plan-shape configuration, and the self-reference walk ranges
//! over **the revision's whole composite set**: §9's acceptance criterion asks
//! for direct *and transitive* self-reference to be rejected, and a transitive
//! cycle is `A → B → A` across two definitions. A row-local rule sees one
//! definition and can only find the direct case, which is the half that matters
//! least.
//!
//! # The third rule of `inst-cm-constituents` is not here, and that is measured
//!
//! *"≥ 2 **published** constituent `meteringUnit` ids (registry-declared)"* — the
//! publication half has no operand. `metering_unit` / `MeteringUnit` appear
//! nowhere in `src/`, `PriceRow::meter` is a free `Option<String>` validated
//! against nothing, and this gear holds no registry client at all
//! ([`crate::domain::ports`] carries the `CatalogVersion` registry and nothing
//! else). `COMPOSITE_CONSTITUENT_UNPUBLISHED` is therefore declared in S10 §5 and
//! **not raised anywhere**, exactly as `plan_rules` names `SKU_NOT_PUBLISHED` and
//! `contracts` names `GRANT_REF_UNDEFINED`: a rule over an empty lookup either
//! refuses every composite ever authored or passes every one, and a rule that
//! always passes is indistinguishable from a rule that holds.
//!
//! `inst-cm-output-unit` is likewise absent because it is not this gear's act at
//! all — declaring the output unit is the registry's (D-32).

use std::collections::{BTreeMap, BTreeSet};

use toolkit_macros::domain_model;

use crate::domain::plan_shape::{CompositeMeter, PlanShape};
use crate::domain::rules::{COMPOSITE_SELF_REFERENCE, COMPOSITE_TOO_FEW_CONSTITUENTS};
use crate::domain::validation::{ValidationReport, ValidationRule};

/// `inst-cm-constituents`, arity half: a composite prices **several** meters.
///
/// One constituent is not a composite — it is the constituent, and rating it
/// through a derived unit adds a level of indirection that changes no charge.
/// Zero is a formula with nothing to evaluate over.
///
/// # It counts **distinct** units, and the difference is the whole rule
///
/// The count is over the distinct constituent set, not over the authored list.
/// `["vcpu", "vcpu"]` has length two and names one meter, so a length test
/// admits precisely the composite this rule exists to refuse — the derived unit
/// over a single constituent — with a duplicate as its disguise. The
/// instruction's own words are "≥ 2 constituent `meteringUnit` **ids**", and two
/// occurrences of one id are one id.
///
/// Nothing else in the gear catches it. The table's unique index is over
/// `(tenant, plan, revision, output_unit)` and `constituent_units` is an opaque
/// `jsonb` array with no `CHECK` over its contents (`pricing_composite_meter`), so the
/// duplicate is storable; and `CompositeSelfReference` walks the same list
/// looking for a cycle, which a repeat is not.
///
/// **The duplicate itself is not refused, only its arity effect.** A composite
/// naming `["vcpu", "vcpu", "ram"]` passes here, because it does price two meters
/// together and the repeat is at worst redundant notation in a formula this crate
/// never evaluates (A4). Refusing redundancy would need a code the design set
/// does not declare.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct CompositeArity;

impl ValidationRule<PlanShape> for CompositeArity {
    fn name(&self) -> &'static str {
        "inst-cm-constituents"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        for composite in &subject.composites {
            let distinct: BTreeSet<&str> = composite
                .constituent_units
                .iter()
                .map(String::as_str)
                .collect();
            if distinct.len() < 2 {
                report.violate(
                    COMPOSITE_TOO_FEW_CONSTITUENTS,
                    composite.output_unit.clone(),
                    format!(
                        "composite meter `{}` names {} distinct constituent unit(s) across {} \
                         entr(y/ies); a derived meter prices two or more meters together, and \
                         one is the constituent itself (inst-cm-constituents)",
                        composite.output_unit,
                        distinct.len(),
                        composite.constituent_units.len()
                    ),
                );
            }
        }
    }
}

/// `inst-cm-formula`, self-reference half: **direct and transitive**.
///
/// The edge is "output unit `X` is built from constituent `Y`". A composite whose
/// own output unit appears among its constituents is the direct case; a cycle
/// across two or more definitions is the transitive one, and §9 asks for both. A
/// cycle means the formula's value is defined in terms of itself, which no
/// evaluation order resolves.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct CompositeSelfReference;

impl ValidationRule<PlanShape> for CompositeSelfReference {
    fn name(&self) -> &'static str {
        "inst-cm-formula"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        // One edge set over the whole revision, because a transitive cycle is
        // not visible from any single definition.
        let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for composite in &subject.composites {
            edges
                .entry(composite.output_unit.as_str())
                .or_default()
                .extend(composite.constituent_units.iter().map(String::as_str));
        }

        for composite in &subject.composites {
            if reaches_itself(composite, &edges) {
                report.violate(
                    COMPOSITE_SELF_REFERENCE,
                    composite.output_unit.clone(),
                    format!(
                        "composite meter `{}` resolves to itself through its constituents; a \
                         formula defined in terms of its own output has no evaluation order \
                         (inst-cm-formula)",
                        composite.output_unit
                    ),
                );
            }
        }
    }
}

/// Does `composite`'s output unit reach itself by following constituent edges?
///
/// A plain depth-first walk with a visited set, which is what makes it total on a
/// cyclic graph: without it, the very input this rule exists to reject would not
/// return.
fn reaches_itself(composite: &CompositeMeter, edges: &BTreeMap<&str, Vec<&str>>) -> bool {
    let target = composite.output_unit.as_str();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = composite
        .constituent_units
        .iter()
        .map(String::as_str)
        .collect();

    while let Some(unit) = stack.pop() {
        if unit == target {
            return true;
        }
        if !seen.insert(unit) {
            continue;
        }
        if let Some(next) = edges.get(unit) {
            stack.extend(next.iter().copied());
        }
    }
    false
}

#[cfg(test)]
#[path = "composite_tests.rs"]
mod composite_tests;
