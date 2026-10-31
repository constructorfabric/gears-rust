//! The Preview result shaping (S3-11; F03 §2.2 step 6, §4.6; D-125, D-216): one gate run's
//! assessment and answers in, the SDK [`PreviewResult`] out. Pure: no call, no storage, no clock.
//!
//! Nothing here computes a figure. Every money value (the resolved total, the TCV, the indicative
//! tax) is the producer's text copied verbatim into its SDK view; `usage_flagged` is whether any
//! evaluated line says so, and the expected fulfillment time is the latest service activation
//! date the run's frozen date basis resolved (a comparison of dates, not arithmetic).
//!
//! - **Per line:** the pin outcome of the assessment and, when the line's resolved service
//!   activation date differs from the run's proposed date, that date as its `deferral`.
//! - **TCV (D-125):** the evaluation's `tcv` only when every line authored both a term and a
//!   billing cycle and none is `tcv-only`; otherwise `tcv` is absent and `tcvWithheld` lists every
//!   line lacking either, plus every line whose pin row the assessment marked
//!   [`Applicability::TcvOnly`] (its billing terms were unavailable or missing), with the reason
//!   [`TCV_WITHHELD_REASON`] (the only registered annotation). A withheld TCV is a
//!   successful-response annotation, never a refusal: every other member is still returned.
//! - **Resolved total and indicative tax:** copied from the evaluation and the tax answer when the
//!   run obtained them, absent otherwise.
use super::answers::{Amount, Availability, ExactMoney, IndicativeTax, OrderEvaluation, Tcv};
use super::assess::Assessment;
use super::predicates::PredicateId;
use super::pricing_map::PinOutcome;
use super::vector::Applicability;
use bss_orders_sdk::authoring::CalendarDate;
use bss_orders_sdk::preview::{
    DiscountView, ExactMoneyView, IndicativeTaxView, LineTax, MoneyView, PinOutcomeView,
    PreviewLineResult, PreviewResult, ResolvedTotalView, TcvWithheld,
};
use time::Date;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// The `tcvWithheld.reason` of a basket with a line lacking term duration or billing cycle
/// (DESIGN reason table, D-125).
pub const TCV_WITHHELD_REASON: &str = "preview-term-or-cycle-missing";

/// What the shaping reads of one basket line, in basket order.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewLineFacts {
    /// The run-local line identity.
    pub line_id: Uuid,
    /// Whether the line authored both a term duration and a billing cycle (D-125).
    pub term_and_cycle_authored: bool,
    /// The line's service activation date as the run's frozen date basis resolved it; `None`
    /// when the line's date cascade did not resolve.
    pub service_activation: Option<Date>,
}

/// The answers of the run the result copies, when the run obtained them.
#[domain_model]
#[derive(Debug, Clone, Copy, Default)]
pub struct PreviewAnswers<'a> {
    /// The Rating evaluation.
    pub evaluation: Option<&'a OrderEvaluation>,
    /// The indicative tax.
    pub tax: Option<&'a IndicativeTax>,
}

/// The Preview result of run `run_id` (module doc). `proposed` is the run's proposed UTC date
/// (`dates::proposed_date` of the frozen basis).
#[must_use]
pub fn preview_result(
    run_id: Uuid,
    assessment: &Assessment,
    answers: PreviewAnswers<'_>,
    lines: &[PreviewLineFacts],
    proposed: Date,
) -> PreviewResult {
    let deferral = |l: &PreviewLineFacts| l.service_activation.filter(|d| *d != proposed);
    let line_results = lines
        .iter()
        .map(|l| {
            PreviewLineResult::new(
                l.line_id,
                pin_view(assessment.pins.get(&l.line_id)),
                deferral(l).map(CalendarDate),
            )
        })
        .collect();
    let mut result = PreviewResult::new(assessment.report(run_id), line_results);
    result.expected_fulfillment_time = lines
        .iter()
        .any(|l| deferral(l).is_some())
        .then(|| lines.iter().filter_map(|l| l.service_activation).max())
        .flatten()
        .map(|d| d.midnight().assume_utc());
    let terms_withheld = |line: Uuid| {
        assessment.vector.outcomes().iter().any(|o| {
            o.key.predicate == PredicateId::PinComposition
                && o.key.line_id == Some(line)
                && (o.applicability == Applicability::TcvOnly
                    || o.reason == Some(bss_orders_sdk::catalog::Reason::BillingTermsUnsupported))
        })
    };
    let withheld: Vec<Uuid> = lines
        .iter()
        .filter(|l| !l.term_and_cycle_authored || terms_withheld(l.line_id))
        .map(|l| l.line_id)
        .collect();
    if withheld.is_empty() {
        let value = answers.evaluation.and_then(|e| e.tcv.as_ref());
        result.tcv = value.map(tcv);
        result.tcv_basis = value
            .and_then(|t| t.basis)
            .map(|basis| basis.token().to_owned());
    } else {
        result.tcv_withheld = Some(TcvWithheld::new(TCV_WITHHELD_REASON, withheld));
    }
    result.resolved_total = answers.evaluation.map(resolved_total);
    result.indicative_tax = answers.tax.map(indicative_tax);
    result
}

/// The public view of a line's catalog pin composition (`Assessment::pins`), independent of the
/// pin-composition row's policy and terms verdict (Q-D3). A line the assessment has no pin for
/// (never the case for a basket line `assess` saw) is unevaluable as an unresolvable reference.
fn pin_view(pin: Option<&PinOutcome>) -> PinOutcomeView {
    match pin {
        Some(PinOutcome::Composed { .. }) => PinOutcomeView::Composed,
        Some(PinOutcome::Unresolvable(reason)) => PinOutcomeView::Unresolvable {
            reason: reason.token().to_owned(),
        },
        Some(PinOutcome::Unevaluable(reason)) => PinOutcomeView::Unevaluable {
            reason: reason.token().to_owned(),
        },
        None => PinOutcomeView::Unevaluable {
            reason: bss_orders_sdk::catalog::Reason::ReferenceUnresolvable
                .token()
                .to_owned(),
        },
    }
}

fn exact(e: &ExactMoney) -> ExactMoneyView {
    ExactMoneyView::new(
        e.numerator().to_owned(),
        e.denominator().to_owned(),
        e.currency().to_owned(),
        e.currency_scale(),
    )
}

fn money(a: &Amount) -> MoneyView {
    MoneyView::new(exact(&a.exact), a.display.clone())
}

/// The TCV's exact value and display text; the posting estimate is not a Preview member.
fn tcv(t: &Tcv) -> MoneyView {
    MoneyView::new(exact(&t.exact), t.display.clone())
}

fn resolved_total(e: &OrderEvaluation) -> ResolvedTotalView {
    let totals = &e.order_totals;
    let discount = match &totals.discount {
        Availability::Available(a) => DiscountView::Available(money(a)),
        Availability::Unavailable => DiscountView::Unavailable,
    };
    let mut total = ResolvedTotalView::new(
        e.currency.clone(),
        e.currency_scale,
        money(&totals.gross),
        money(&totals.net),
        discount,
        e.lines.iter().any(|l| l.usage_flagged),
    );
    total.exclusions = e.order_declaration.as_ref().map(|d| d.exclusions.clone());
    total
}

fn indicative_tax(t: &IndicativeTax) -> IndicativeTaxView {
    IndicativeTaxView::new(
        t.per_line
            .iter()
            .map(|l| LineTax::new(l.line_id, money(&l.amount)))
            .collect(),
        money(&t.total),
        t.explicit_no_tax,
    )
}
