//! Model-kind rules (`cpt-cf-bss-pricing-algo-model-kind`).
//!
//! Four rules: the kind is explicit, its required fields are present, its
//! forbidden fields are absent, and the kind is legal on the row's charge
//! component.
//!
//! The three field rules all start by reading `model_kind` and returning when it
//! is absent. That is deliberate rather than defensive: `inst-mk-explicit` has
//! already recorded `MODEL_KIND_MISSING`, and re-deriving a per-kind field set
//! from a kind nobody authored would fill the report with consequences of the
//! one fault the author has to fix first.

use bss_fixtures::ModelKind;
use toolkit_macros::domain_model;

use crate::domain::price_row::{PriceRow, QuantitySource, model_kind_wire};
use crate::domain::rules::{
    AMOUNT_PLACEMENT_INVALID, EVAL_POLICY_MISPLACED, MODEL_KIND_CHARGEKIND_MISMATCH,
    MODEL_KIND_MISSING, PACKAGE_FIELDS_INVALID, QUANTITY_SOURCE_MISSING,
};
use crate::domain::validation::{ValidationReport, ValidationRule};

/// `inst-mk-explicit` — the kind is authored, never inferred.
///
/// There is no implicit default at rating time, so "tiered (unspecified)" is not
/// a publishable row: a kind the catalog leaves open is a formula Tariffs would
/// have to guess.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct ExplicitModelKind;

impl ValidationRule<PriceRow> for ExplicitModelKind {
    fn name(&self) -> &'static str {
        "inst-mk-explicit"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        if subject.model_kind.is_none() {
            report.violate(
                MODEL_KIND_MISSING,
                subject.subject(),
                "modelKind must be one of flat | per_unit | graduated | volume | package; \
                 no implicit default exists at rating time",
            );
        }
    }
}

/// `inst-mk-required` — the fields a kind cannot be evaluated without.
///
/// Owns the **amount-placement matrix**: `amount_minor` is where `flat` keeps
/// its single amount; `unit_rate` is where `per_unit` keeps its rate (**D-311** —
/// it was `amount_minor` too, one column meaning an amount on one
/// kind and a multiplier on another); the band kinds keep money in
/// `pricing_price_tier_band.unit_price_nano`; and `package` keeps it in
/// `package_price_minor`. Each kind carries exactly one of them and NULL in the
/// rest, so no row ever carries two competing prices.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct KindRequiredFields;

impl ValidationRule<PriceRow> for KindRequiredFields {
    fn name(&self) -> &'static str {
        "inst-mk-required"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        let Some(kind) = subject.model_kind else {
            return;
        };
        let wire = model_kind_wire(kind);
        check_amount_placement(subject, kind, report);

        if matches!(kind, ModelKind::PerUnit) && !subject.is_usage() {
            check_quantity_source(subject, report);
        }

        if matches!(kind, ModelKind::Package)
            && (subject.package_size.is_none() || subject.package_price_minor.is_none())
        {
            report.violate(
                PACKAGE_FIELDS_INVALID,
                subject.subject(),
                format!(
                    "a {wire} row must carry both package_size and package_price_minor; \
                     the block size and the block price are what it prices with"
                ),
            );
        }
    }
}

/// Which column each kind keeps its money in.
///
/// Four columns, and after D-311 two of them are **amounts** and two are
/// **rates**: `flat` charges a sum from `amount_minor`, `per_unit` multiplies a
/// rate from `unit_rate`, the band kinds multiply band rates, and `package`
/// charges a block price. The matrix is written as one `match` on the kind rather
/// than as a pair of booleans, because the boolean spelling is what let `per_unit`
/// and `flat` share an arm — and sharing that arm is the defect D-311 names.
///
/// **Both directions are checked for both columns.** A row missing the column its
/// kind prices from has no price; a row carrying the *other* one has two, which is
/// the "two competing prices" the rule has always been about. Before this pair was
/// separated, a `per_unit` row authored with a rate was refused for an absent
/// `amount_minor` while a row authored the old way passed with its rate column
/// empty — the new column unauthorable and the old spelling still accepted.
fn check_amount_placement(row: &PriceRow, kind: ModelKind, report: &mut ValidationReport) {
    let wire = model_kind_wire(kind);
    let (wants_amount, wants_rate) = match kind {
        ModelKind::Flat => (true, false),
        ModelKind::PerUnit => (false, true),
        ModelKind::Graduated | ModelKind::Volume | ModelKind::Package => (false, false),
    };

    check_money_column(
        row,
        report,
        wants_amount,
        row.amount_minor.is_some(),
        &format!("a {wire} row keeps its money in amount_minor, and it is absent"),
        &format!(
            "amount_minor must be NULL on a {wire} row: its money lives in the rate, band or \
             package column, and two priced columns are two competing prices"
        ),
    );
    check_money_column(
        row,
        report,
        wants_rate,
        row.unit_rate.is_some(),
        &format!(
            "a {wire} row prices by a rate and keeps it in unitRateNanoMinor, and it is absent"
        ),
        &format!(
            "unitRateNanoMinor must be NULL on a {wire} row: a rate is a multiplier and this \
             kind charges no per-unit multiple"
        ),
    );
}

/// One column of the matrix: required and absent, or forbidden and present.
fn check_money_column(
    row: &PriceRow,
    report: &mut ValidationReport,
    required: bool,
    present: bool,
    when_absent: &str,
    when_forbidden: &str,
) {
    match (required, present) {
        (true, false) => report.violate(AMOUNT_PLACEMENT_INVALID, row.subject(), when_absent),
        (false, true) => report.violate(AMOUNT_PLACEMENT_INVALID, row.subject(), when_forbidden),
        (true, true) | (false, false) => {}
    }
}

/// A non-usage `per_unit` row states where its quantity comes from — and, when
/// it states `manual`, what that quantity is.
fn check_quantity_source(row: &PriceRow, report: &mut ValidationReport) {
    match row.quantity_source {
        None => report.violate(
            QUANTITY_SOURCE_MISSING,
            row.subject(),
            "a non-usage per_unit row must declare quantitySource \
             (subscription_seat_count | manual): nothing meters it",
        ),
        Some(QuantitySource::Manual) if row.manual_quantity.is_none() => report.violate(
            QUANTITY_SOURCE_MISSING,
            row.subject(),
            "quantitySource = manual requires manual_quantity: the fixed quantity is the \
             whole of what `manual` supplies",
        ),
        Some(_) => {}
    }
}

/// `inst-mk-forbidden` — fields on a row whose kind and charge component do not
/// admit them.
///
/// Two families. **Tier bands** are authorable only where the money lives in
/// them (`graduated` / `volume`); on `flat` and `per_unit` they are a second,
/// unreachable price. **Evaluation policy** (`tierAggregationWindow`,
/// `billingGranularity`) and **`quantitySource`** are mutually exclusive answers
/// to "where does `Q` come from": the first two are usage-row-only, and
/// `quantitySource` is legal only on a `per_unit` row that is *not* usage — a
/// `per_unit` usage row takes its quantity from the meter.
///
/// The `package` x bands case is **not** here: it is structural exclusivity
/// between two ways of pricing blocks and it belongs to `inst-pk-fields`, which
/// reports it under `PACKAGE_FIELDS_INVALID`. Reporting it twice under two codes
/// would make one authoring mistake look like two.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct KindForbiddenFields;

impl ValidationRule<PriceRow> for KindForbiddenFields {
    fn name(&self) -> &'static str {
        "inst-mk-forbidden"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        // **The checks that do not read `model_kind` run before the kind is
        // required.** Only two of the faults below are kind-dependent — bands on a
        // `flat` / `per_unit` row, and `quantitySource` on a non-usage row of the
        // wrong kind. The rest judge content against the row's `chargeKind` alone,
        // and an early return on an absent kind hid them at *both* stages: a row
        // carrying `billingGranularity` on a recurring key was answered 201 by the
        // D-312 write door and reported nothing at publish either, though its two
        // operands were both present and one of them frozen. That is the larger of
        // the two populations D-312 counted (nine rows of eleven).
        if !subject.is_usage() {
            check_usage_only_policy(subject, report);
        }
        check_usage_key_quantity_source(subject, report);
        check_manual_quantity_pairing(subject, report);

        let Some(kind) = subject.model_kind else {
            return;
        };
        let wire = model_kind_wire(kind);

        if matches!(kind, ModelKind::Flat | ModelKind::PerUnit) && !subject.bands.is_empty() {
            // Write stage under D-312's amendment: only a valid
            // combination may be stored, so there is no half-authored band
            // ladder to protect.
            //
            // Deferred until then on the argument that both operands are content
            // and a later `model_kind: graduated` would resolve it by adding
            // intent. The state that argument protects cannot be reached: a probe
            // against the deployed platform found `PATCH`ing bands onto a stored
            // `flat` row answers 500, exactly as creating the pair in one call
            // does, because `trg_pricing_price_tier_band_kind` refuses the band
            // insert by either route.
            report.violate_at_write(
                EVAL_POLICY_MISPLACED,
                subject.subject(),
                format!(
                    "tier bands are authorable only on graduated / volume rows; a {wire} row \
                     carrying {} band(s) prices from a column nothing reads",
                    subject.bands.len()
                ),
            );
        }

        check_quantity_source_placement(subject, kind, report);
    }
}

/// `tierAggregationWindow` / `billingGranularity` are usage-row-only: a
/// non-usage row has no metered quantity for them to bound or quantize.
fn check_usage_only_policy(row: &PriceRow, report: &mut ValidationReport) {
    let mut misplaced = Vec::new();
    if row.tier_aggregation_window.is_some() {
        misplaced.push("tierAggregationWindow");
    }
    if row.billing_granularity.is_some() {
        misplaced.push("billingGranularity");
    }
    if misplaced.is_empty() {
        return;
    }
    // D-312: the misplaced field and the frozen `chargeKind` are both present.
    // Its sibling in this file — tier bands on a `flat` row, same code — is
    // write-stage as well, under D-312's amendment.
    //
    // No write-stage exception for "both operands are content, and
    // `model_kind: graduated` resolves it by adding intent rather than by
    // retracting": the intermediate state that would protect is unreachable —
    // `trg_pricing_price_tier_band_kind` refuses a band insert on a `flat` row by
    // either route. Same code on both arms, and no stage split to point at.
    report.violate_at_write(
        EVAL_POLICY_MISPLACED,
        row.subject(),
        format!(
            "{} are usage-row only; this row's chargeKind is {}",
            misplaced.join(" and "),
            row.charge_kind
        ),
    );
}

/// `quantitySource` on a **usage** key — the half of its placement rule that the
/// frozen key decides on its own (D-312).
///
/// A usage row takes its quantity from the meter under *every* model kind, so this
/// fault needs no kind to judge and no later call can add one that legalises it:
/// `chargeKind` is frozen and the only resolution is retracting the field just
/// sent. Its mirror image — `quantitySource` on a non-usage row whose kind is not
/// `per_unit` — is judged at the write as well under D-312's
/// amendment; [`check_quantity_source_placement`] holds it and states the
/// argument, which is that the store carried no constraint for that pair, so it
/// was stored happily and read back as authorable while the band ladder was a
/// 500 — an asymmetry of the schema, not of the faults.
///
/// What separates the two functions is not the stage — they are not "the same
/// code, the same rule, opposite stages". This half needs no `model_kind` at all,
/// which is why it runs ahead of the kind gate and its mirror image runs behind
/// it.
fn check_usage_key_quantity_source(row: &PriceRow, report: &mut ValidationReport) {
    if row.is_usage() && row.quantity_source.is_some() {
        report.violate_at_write(
            EVAL_POLICY_MISPLACED,
            row.subject(),
            format!(
                "quantitySource is authorable only on a non-usage per_unit row; this row's \
                 chargeKind is {} and a metered row takes its quantity from the meter",
                row.charge_kind
            ),
        );
    }
}

/// `manual_quantity` and `quantitySource = manual` are both-or-neither. Two
/// content fields, so publish-stage — but read from no `model_kind`, which is why
/// it is not behind the kind gate.
fn check_manual_quantity_pairing(row: &PriceRow, report: &mut ValidationReport) {
    if row.manual_quantity.is_some() && row.quantity_source != Some(QuantitySource::Manual) {
        report.violate(
            EVAL_POLICY_MISPLACED,
            row.subject(),
            "manual_quantity is authorable if and only if quantitySource = manual; \
             otherwise it is a frozen quantity nothing reads",
        );
    }
}

/// `quantitySource` belongs to exactly one shape: a `per_unit` row that is not a
/// usage row. The usage-key half of that lives in
/// [`check_usage_key_quantity_source`]; what is left here is the kind-dependent
/// half, which is why this still takes a `kind`.
fn check_quantity_source_placement(row: &PriceRow, kind: ModelKind, report: &mut ValidationReport) {
    if row.is_usage() {
        // Already judged, at the write stage, without needing the kind.
        return;
    }
    if row.quantity_source.is_some() && !matches!(kind, ModelKind::PerUnit) {
        // Write stage under D-312's amendment, with its sibling above.
        //
        // The store carries no constraint for this pair, so unlike the band
        // ladder it was stored happily and read back as though it were
        // authorable. That asymmetry is the amendment's own argument: the schema
        // catches some invalid combinations and not others, so the rule and not
        // the constraint has to be what decides.
        report.violate_at_write(
            EVAL_POLICY_MISPLACED,
            row.subject(),
            format!(
                "quantitySource is authorable only on a non-usage per_unit row; on a {} {} row \
                 the quantity comes from the meter",
                row.charge_kind,
                model_kind_wire(kind)
            ),
        );
    }
}

/// `inst-mk-chargekind` — the kind x chargeKind legality matrix (D-18).
///
/// `flat` and `per_unit` are legal on **non-usage** rows. `per_unit`,
/// `graduated`, `volume` and `package` are legal on **usage** rows — a
/// `per_unit` usage row being the plain untiered metered rate, unit price times
/// metered `Q`.
///
/// The two refusals have one reason: the tier and block machinery presupposes a
/// metered quantity stream, and no `Q` semantics exist for a non-usage row. So
/// `flat` on a usage row (a metered row that ignores its meter) and
/// `graduated` / `volume` / `package` on a `recurring` / `one_time` /
/// `one_time_setup` row (bands over a quantity nothing produces) both fail
/// publish. Tiered per-seat pricing is Future scope.
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct KindChargeKindMatrix;

impl ValidationRule<PriceRow> for KindChargeKindMatrix {
    fn name(&self) -> &'static str {
        "inst-mk-chargekind"
    }

    fn evaluate(&self, subject: &PriceRow, report: &mut ValidationReport) {
        let Some(kind) = subject.model_kind else {
            return;
        };
        let legal = if subject.is_usage() {
            matches!(
                kind,
                ModelKind::PerUnit | ModelKind::Graduated | ModelKind::Volume | ModelKind::Package
            )
        } else {
            matches!(kind, ModelKind::Flat | ModelKind::PerUnit)
        };
        if legal {
            return;
        }
        // D-312: both operands are in the request and `chargeKind` is frozen by
        // the scope key, so this row is knowably unpublishable the instant it
        // arrives — the authoring write refuses it too.
        report.violate_at_write(
            MODEL_KIND_CHARGEKIND_MISMATCH,
            subject.subject(),
            format!(
                "modelKind {} is not legal on a {} row: flat and per_unit are the non-usage \
                 kinds, and per_unit / graduated / volume / package are the usage kinds",
                model_kind_wire(kind),
                subject.charge_kind
            ),
        );
    }
}

#[cfg(test)]
#[path = "model_kind_tests.rs"]
mod model_kind_tests;
