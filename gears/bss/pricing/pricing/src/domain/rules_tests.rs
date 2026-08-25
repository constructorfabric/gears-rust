//! Tests for the row-local rule registration (Slice 3's set, plus Slice 10's
//! `inst-rv-attrs` since 2026-08-08).

use bss_fixtures::ModelKind;

use super::{
    EVAL_POLICY_MISSING, MODEL_KIND_MISSING, SUPERSESSION_UNIT_MISMATCH, SupersessionPair,
    TIER_TOP_CLOSED, price_row_rules, supersession_rules,
};
use crate::domain::money::RateMinor;
use crate::domain::price_row::{BillingGranularity, PriceRow, TierAggregationWindow, TierBand};
use crate::domain::scope_key::ChargeKind;

/// A band rate, stated in whole minor units so these cases read as they
/// always did (D-311). The stored scale is 10^-9 of one.
fn rate(minor_units: i64) -> RateMinor {
    RateMinor::from_minor_units(minor_units).expect("test rate is non-negative")
}

fn graduated_usage() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Graduated));
    row.meter = Some("api_calls".to_owned());
    row.billing_granularity = Some(BillingGranularity::WholeUnit);
    row.tier_aggregation_window = Some(TierAggregationWindow::CalendarMonth);
    row.bands = vec![
        TierBand::closed(0, 1_000, rate(10)),
        TierBand::open(1_000, rate(6)),
    ];
    row
}

#[test]
fn the_pipeline_registers_every_row_local_instruction() {
    // The instruction ids are the design set's own names; the registration list
    // is what makes a missing rule visible as an absent id rather than as a row
    // that quietly publishes.
    //
    // **It stopped being a Slice-3-only list on 2026-08-08.** `inst-rv-attrs` is
    // Slice 10's, and it is registered here rather than in the Foundation plan
    // set because a reservation is judged against one row and nothing else --
    // D-21's test for the save-time set -- and because the joint corpus reaches
    // this pipeline and never assembles a `PlanShape`. The test was renamed with
    // it: a roster whose name says "slice three" is a roster a later slice
    // hesitates to extend.
    //
    // `inst-ac-gate` and `inst-ac-band` joined on the same terms (D-45): the
    // allowance gate judges one row against itself, and the compiled-set check
    // judges the ladder that row projects. Both are here rather than in the
    // Foundation plan set, and the corpus reaches both.
    assert_eq!(
        price_row_rules().rule_names(),
        vec![
            "inst-mk-explicit",
            "inst-mk-required",
            "inst-mk-forbidden",
            "inst-mk-chargekind",
            "inst-tb-order",
            "inst-tb-first",
            "inst-tb-top",
            "inst-tb-window",
            "inst-pk-fields",
            "inst-pk-window",
            "inst-la-fields",
            "inst-la-granularity",
            "inst-la-maxhold",
            "inst-rv-attrs",
            "inst-ac-gate",
            "inst-ac-band",
            "inst-ft-fallback",
            "inst-ft-warn",
        ]
    );
    assert_eq!(
        supersession_rules().rule_names(),
        vec!["inst-tb-supersession-units"]
    );
}

#[test]
fn a_well_formed_graduated_usage_row_publishes() {
    let report = price_row_rules().run(&graduated_usage());

    assert!(
        report.is_publishable(),
        "unexpected violations: {:?}",
        report
            .violations
            .iter()
            .map(|violation| violation.code.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn one_row_reports_every_fault_it_carries() {
    // The aggregate report is the point: an author remediates a plan in one
    // pass, so an unspecified kind must not mask the band and policy faults.
    let mut row = graduated_usage();
    row.model_kind = None;
    row.billing_granularity = None;
    row.bands = vec![TierBand::closed(0, 1_000, rate(10))];

    let report = price_row_rules().run(&row);
    let codes: Vec<&str> = report
        .violations
        .iter()
        .map(|violation| violation.code.as_str())
        .collect();

    assert!(codes.contains(&MODEL_KIND_MISSING));
    assert!(codes.contains(&TIER_TOP_CLOSED));
    assert!(codes.contains(&EVAL_POLICY_MISSING));
}

#[test]
fn the_supersession_pipeline_judges_a_pair_without_asking_which_mechanism_made_it() {
    // D-127: both sanctioned producers of `published -> superseded` run this
    // same pipeline over this same subject, so there is no argument by which a
    // cutover successor could be judged more leniently than an interactive one.
    let mut successor = graduated_usage();
    successor.billing_granularity = Some(BillingGranularity::PerDay);

    let report = supersession_rules().run(&SupersessionPair::new(graduated_usage(), successor));

    assert_eq!(report.violations.len(), 1);
    assert_eq!(report.violations[0].code, SUPERSESSION_UNIT_MISMATCH);
}

/// D-312's group probe: over the **whole** registered set, which violations the
/// authoring write may judge.
///
/// Armed against the claim the change actually makes, not against half of it. A
/// probe that only asserted "the four faults are write-stage" would pass against
/// an implementation that had marked every rule — and that implementation refuses
/// an author's legitimate half-built row, which no gate here would report. So both
/// halves are asserted: exactly these codes, and *nothing else from a row that is
/// merely incomplete*.
mod write_stage_over_the_whole_set {
    use super::*;
    use crate::domain::money::MinorAmount;
    use crate::domain::price_row::{AggregationFunction, QuantitySource, ReservationFlavor};
    use crate::domain::rules::reservation::RESERVATION_ON_NON_USAGE;
    use crate::domain::rules::{
        EVAL_POLICY_MISPLACED, LEVEL_FIELDS_INVALID, MODEL_KIND_CHARGEKIND_MISMATCH,
        PACKAGE_FIELDS_INVALID,
    };

    fn minor(units: i64) -> MinorAmount {
        MinorAmount::new(units).expect("test amount is non-negative")
    }

    fn write_codes(row: &PriceRow) -> Vec<String> {
        price_row_rules()
            .run(row)
            .write_stage_only()
            .map(|report| {
                let mut codes: Vec<String> =
                    report.violations.iter().map(|v| v.code.clone()).collect();
                codes.sort();
                codes.dedup();
                codes
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_non_usage_row_carrying_every_misplaced_field_reports_three() {
        // One recurring row cannot also be the `flat`-on-usage case — that fault
        // needs the opposite charge kind — so the four are proved across two rows.
        let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
        row.amount_minor = Some(minor(2_500));
        row.billing_granularity = Some(BillingGranularity::WholeUnit);
        row.aggregation_function = Some(AggregationFunction::Peak);
        row.reserved_rate = Some(RateMinor::from_minor_units(3).expect("a non-negative rate"));
        row.reservation_flavor = Some(ReservationFlavor::Capacity);
        assert_eq!(
            write_codes(&row),
            vec![
                EVAL_POLICY_MISPLACED.to_owned(),
                LEVEL_FIELDS_INVALID.to_owned(),
                RESERVATION_ON_NON_USAGE.to_owned(),
            ]
        );
    }

    #[test]
    fn a_kind_illegal_on_its_charge_kind_is_the_fourth() {
        let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Flat));
        row.amount_minor = Some(minor(250));
        row.meter = Some("addon_dr".to_owned());
        row.billing_granularity = Some(BillingGranularity::WholeUnit);
        assert_eq!(
            write_codes(&row),
            vec![MODEL_KIND_CHARGEKIND_MISMATCH.to_owned()]
        );
    }

    #[test]
    fn a_row_whose_kind_is_not_yet_picked_still_answers_for_its_frozen_key() {
        // The population D-312 counted, over the whole set: nine of the eleven
        // stored contradictions are `billingGranularity` on a recurring row, and
        // the Studio's `defaultContent` wrote them with the model-kind picker
        // untouched. Judged here because the fault's operands are the field and the
        // frozen `chargeKind`; the kind is absent from the *fault*, not merely from
        // the row. The rule used to return early on the missing kind, so the check
        // this decision built passed the very rows it was measured against.
        let mut row = PriceRow::new(ChargeKind::Recurring, None);
        row.billing_granularity = Some(BillingGranularity::WholeUnit);
        assert_eq!(write_codes(&row), vec![EVAL_POLICY_MISPLACED.to_owned()]);
    }

    /// `inst-pk-fields` was the one member of the frozen-key family with no such
    /// arm (review B3-1), so the two package fields beside a frozen non-usage
    /// `chargeKind` were judged at **neither** stage until a model kind arrived.
    ///
    /// The row below is the population the sibling case above describes, one
    /// field over: `package_size` on a `recurring` key with the kind picker
    /// untouched. Its only two readers in the whole rule set —
    /// `inst-mk-required` and `inst-pk-fields` — both sat behind
    /// `let Some(kind) = subject.model_kind else { return }`, and `inst-pk-window`
    /// early-returns on `!is_usage()`, so the report named `MODEL_KIND_MISSING`
    /// and nothing at all about the package fields. Setting any kind then surfaced
    /// it at publish only, under a message naming the *kind* — which points the
    /// author at `model_kind: package`, the one value `inst-mk-chargekind` then
    /// refuses.
    #[test]
    fn package_fields_on_a_frozen_non_usage_key_answer_before_a_kind_is_picked() {
        let mut row = PriceRow::new(ChargeKind::Recurring, None);
        row.package_size = Some(100);
        assert_eq!(write_codes(&row), vec![PACKAGE_FIELDS_INVALID.to_owned()]);

        // The other field alone reaches it too, and both together report once.
        let mut price_only = PriceRow::new(ChargeKind::OneTime, None);
        price_only.package_price_minor = Some(minor(900));
        assert_eq!(
            write_codes(&price_only),
            vec![PACKAGE_FIELDS_INVALID.to_owned()]
        );

        // And the positive control the arm must not over-refuse: the same key
        // with neither field is a legitimate half-built row.
        let bare = PriceRow::new(ChargeKind::Recurring, None);
        assert!(
            price_row_rules().run(&bare).write_stage_only().is_none(),
            "a recurring row that authored no package field must still save"
        );
    }

    /// The kind-present half, kept beside the case above so the two readings of
    /// one fault cannot drift apart: a non-usage row that *has* picked a kind
    /// reports the same code once, at the write, and not twice.
    #[test]
    fn package_fields_beside_a_picked_non_usage_kind_report_once() {
        let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
        row.amount_minor = Some(minor(2_500));
        row.package_size = Some(100);

        assert_eq!(write_codes(&row), vec![PACKAGE_FIELDS_INVALID.to_owned()]);

        let all = price_row_rules().run(&row);
        assert_eq!(
            all.violations
                .iter()
                .filter(|v| v.code == PACKAGE_FIELDS_INVALID)
                .count(),
            1,
            "the hoisted write arm and the publish arm below it are two spellings of one \
             fault, not two faults: {:?}",
            all.violations
        );
    }

    #[test]
    fn a_quantity_source_on_a_usage_key_is_the_fifth_fault() {
        let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::PerUnit));
        row.meter = Some("addon_dr".to_owned());
        row.unit_rate = Some(rate(3));
        row.quantity_source = Some(QuantitySource::SubscriptionSeatCount);
        assert_eq!(write_codes(&row), vec![EVAL_POLICY_MISPLACED.to_owned()]);
    }

    #[test]
    fn a_merely_incomplete_row_is_judged_by_nothing_at_the_write() {
        // The half that matters most. This row fails publish several times over —
        // no kind, no amount, no bands — and every one of those faults is
        // completed by a later call. If any of them reached the write, multi-call
        // authoring would be broken and only this assertion would say so.
        let row = PriceRow::new(ChargeKind::Usage, None);
        assert!(
            price_row_rules().run(&row).write_stage_only().is_none(),
            "an incomplete row must still save: {:?}",
            write_codes(&row)
        );
        assert!(
            !price_row_rules().run(&row).violations.is_empty(),
            "the probe proves nothing unless this row does fail publish"
        );
    }

    #[test]
    fn a_publishable_row_is_judged_by_nothing_at_the_write() {
        assert!(
            price_row_rules()
                .run(&graduated_usage())
                .write_stage_only()
                .is_none()
        );
    }
}
