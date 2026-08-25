//! Billing descriptor completeness (`cpt-cf-bss-pricing-algo-descriptors`).
//!
//! One rule over a [`PlanShape`]: the descriptor set a publish would freeze
//! carries every element the **tenant** requires (D-48's pinned three, plus
//! whatever their `pricing_policy_object` entry adds — D-152), and the report
//! names **each** missing one rather than the first. `inst-ds-required` says
//! "publish blocks on any missing element with the field named", and an author
//! remediates a plan in one pass — a rule that stopped at the first absence
//! would send them round the pipeline once per missing field.
//!
//! ## Three of D-48 v1's five elements, and the other two have owners
//!
//! D-48 pinned a five-element v1 contract. Two of the five are **row-borne** and
//! are deliberately not checked here — this is Global Constraint 5's third arm,
//! and the whole of the reason:
//!
//! - **`billingTiming` on every recurring row is Slice 6's registered rule.**
//!   `inst-cs-recurring` says so in as many words ("cross-referenced here, never
//!   re-registered; single owner, review fix").
//! - **`taxCategory` is each row's `tax_category_ref`** (Slice 4
//!   `inst-td-persist`, sole source of truth per D-110). A per-plan mirror of it
//!   was removed precisely because the consistency check meant to reconcile the
//!   two was undefined whenever two rows of one plan carried different
//!   categories.
//!
//! `inst-ds-required` does say publish blocks "with the row named" for those
//! two. **That naming belongs to their owners' rules**: re-checking either here
//! would be a second owner of one rule, free to disagree with the first, and the
//! disagreement would be invisible — both readings look right at their own call
//! site. What this rule owns is the three descriptor-set fields plus whatever a
//! tenant adds to them.
//!
//! **That gap is now closed** (Slice 4, `pricing_price`): `pricing_price`
//! carries `tax_category_ref`, and the owner this rule defers to is
//! `domain::tax_display::TaxBasisComplete`, whose category arm blocks publish
//! unconditionally when the effective category resolves to nothing (D-154).
//!
//! ## `inst-ds-sufficient` is not a rule here
//!
//! "The frozen set MUST be sufficient for Billing/ERP to post without
//! re-querying mutable rows" is a property of the **frozen snapshot** — of what
//! the publish unit writes and what a consumer can resolve from it — not of the
//! draft this pipeline judges. It belongs to the read-model projection (G6). A
//! rule here could only re-assert the required-set it already checks, which is
//! the shape of a rule that always passes.

use std::collections::BTreeSet;

use toolkit_macros::domain_model;

use crate::domain::plan_rules::DESCRIPTOR_INCOMPLETE;
use crate::domain::plan_shape::{DescriptorSet, PlanShape};
use crate::domain::validation::{ValidationReport, ValidationRule};

/// The v1 required set (D-48 as recomposed by D-110), in the order the design
/// set names the fields.
///
/// The spelling is the wire spelling every other rule's detail uses
/// (`planTier`, `billingGranularity`), because the name in the report is what an
/// author looks for in the payload they submitted — not the column name, which
/// they never see.
pub const V1_REQUIRED_DESCRIPTORS: [&str; 3] = [V1_COLUMNS[0].0, V1_COLUMNS[1].0, V1_COLUMNS[2].0];

/// Each v1 descriptor's wire name beside the column it is read from.
///
/// **One roster, because there were two.** The names were spelled here and again
/// as `match` arms in [`declared`], with nothing tying either to
/// [`DescriptorSet`]'s own fields — so a fourth column could be added, given a
/// name in one place and read from neither, or read and never required.
/// `every_v1_column_is_in_the_roster` destructures the struct exhaustively, which
/// is what makes a new field a failure here rather than a silent absence.
type ColumnRead = for<'a> fn(&'a DescriptorSet) -> Option<&'a str>;
const V1_COLUMNS: [(&str, ColumnRead); 3] = [
    ("invoiceLineTemplate", |set| {
        set.invoice_line_template.as_deref()
    }),
    ("glCode", |set| set.gl_code.as_deref()),
    ("itemizationRule", |set| set.itemization_rule.as_deref()),
];

/// `inst-ds-required` — the descriptor set is complete at publish.
///
/// **The required set is a field, not a constant read** (P5: "the validator's
/// required-set is config-extensible without a schema change"). A tenant that
/// must require a fourth descriptor names it in their `pricing_policy_object`
/// entry (D-152) and carries its value in [`DescriptorSet::additional`], so the
/// requirement costs no migration. **No surface in this gear writes that column
/// yet**, so the fourth key is declarable and not yet authorable and this rule's
/// extension set is empty for every tenant — the carrier and its per-column
/// consequence are set out in
/// [`policy_repo`](crate::infra::storage::repo::policy_repo)'s module doc.
/// Holding the list here rather than reaching
/// for that entry inside `evaluate` is [`crate::domain::validation`]'s purity
/// requirement: the same rule set runs twice — as the §4.2 step-2 pre-check and
/// again inside the publish commit — and a rule that read a policy row itself
/// could answer differently in the two runs for no authored reason.
///
/// **There is exactly one door in, and it can only add** (D-152:
/// additive-only over the pinned v1 three). The required set is not
/// constructible from an arbitrary list, because such a list can be one that
/// *omits* a v1 element — and a per-tenant policy that could drop `glCode`
/// would publish past a pinned element of the contract Billing countersigns,
/// which is a decision no tenant holds. Every extension therefore starts from
/// [`V1_REQUIRED_DESCRIPTORS`] and the configuration only says what comes after
/// it.
///
/// **It has a `Default`, and [`CustomIntervalBounds`] deliberately does not.**
/// The contrast is not an inconsistency: the absent configuration here means
/// "no extension", so the default is the *documented v1 contract*, three named
/// fields D-48 pins. A defaulted interval cap would be `0`, which rejects every
/// custom frequency ever authored while looking exactly like a rule that is
/// switched on. A default is safe when the absent configuration has a meaning;
/// it is a trap when it does not.
///
/// [`CustomIntervalBounds`]: crate::domain::plan_rules::cycle_shape::CustomIntervalBounds
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescriptorSetComplete {
    /// The field names a publish requires, in report order.
    required: Vec<String>,
}

impl Default for DescriptorSetComplete {
    fn default() -> Self {
        Self::extending_v1(Vec::new())
    }
}

impl DescriptorSetComplete {
    /// The rule bound to one tenant's **extension** of the pinned v1 set.
    ///
    /// The v1 three come first and `additional` follows, so the report lists
    /// what D-48 pins in the order D-48 states it and the tenant's own
    /// requirements after. Duplicates are dropped and first-seen order is kept:
    /// an entry that names one key twice — or names a v1 field again — is a
    /// typo, and reporting the same absence twice would read as two distinct
    /// faults to whoever has to fix it. Naming a v1 field also cannot move it,
    /// which is what keeps [`declared`] the single answer to where a descriptor
    /// lives: a field with a column of its own is never satisfied from the
    /// extras.
    #[must_use]
    pub fn extending_v1(additional: impl IntoIterator<Item = String>) -> Self {
        let mut seen = BTreeSet::new();
        Self {
            required: V1_REQUIRED_DESCRIPTORS
                .iter()
                .map(|name| (*name).to_owned())
                .chain(additional)
                .filter(|name| seen.insert(name.clone()))
                .collect(),
        }
    }

    /// The field names this rule requires, in report order.
    #[must_use]
    pub fn required(&self) -> &[String] {
        &self.required
    }
}

impl ValidationRule<PlanShape> for DescriptorSetComplete {
    fn name(&self) -> &'static str {
        "inst-ds-required"
    }

    fn evaluate(&self, subject: &PlanShape, report: &mut ValidationReport) {
        // An absent set and a set whose fields are all blank are the same
        // finding, reported the same way: the author is owed the list of what
        // they still have to supply, and "you attached nothing" is not a shorter
        // way of saying it. Every required field is reported, never the first.
        for field in &self.required {
            if subject
                .descriptor_set
                .as_ref()
                .and_then(|set| declared(set, field))
                .is_some()
            {
                continue;
            }
            report.violate(
                DESCRIPTOR_INCOMPLETE,
                subject.subject(),
                format!(
                    "{field} is required before publish and is missing: the descriptor set \
                     freezes into the catalog version Billing posts from, so an absence here \
                     is one an ERP meets at posting time rather than one an author meets now"
                ),
            );
        }
    }
}

/// The declared value of `field`, when the set carries one.
///
/// The three v1 names resolve to their own columns and everything else resolves
/// through [`DescriptorSet::additional`] — which is what makes the required set
/// extensible without a schema change (P5), and what keeps a tenant from
/// satisfying `glCode` by putting a `glCode` key in the extras: a field with a
/// column of its own is read from that column and from nowhere else, so one
/// descriptor never has two homes.
///
/// **A blank value is not a declaration.** A whitespace-only string is an
/// absence wearing a value's shape, the same reading `PlanTierDeclared` gives
/// the tier, and here it matters more than there: a blank `glCode` freezes into
/// the snapshot and an ERP posts against it.
fn declared<'a>(set: &'a DescriptorSet, field: &str) -> Option<&'a str> {
    let value = match V1_COLUMNS.iter().find(|(name, _)| *name == field) {
        Some((_, read)) => read(set),
        None => set.additional.get(field).map(String::as_str),
    }?;
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(test)]
#[path = "descriptor_set_tests.rs"]
mod descriptor_set_tests;
