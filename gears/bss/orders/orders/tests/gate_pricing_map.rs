#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
//! D-187 typed Pricing/Products read mapping (DESIGN `#contract-03-4-1`,
//! `#contract-03-pricing-read-mapping`): one test per mapping rule.
use bss_orders::domain::gate::ports::{PortAnswer, Unavailable};
use bss_orders::domain::gate::predicates::PredicateId;
use bss_orders::domain::gate::pricing_map::*;
use bss_orders::domain::gate::vector::{Outcome, Vector, Verdict};
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::gate_report::CatalogSelection;
use bss_pricing_sdk::read::*;
use bss_pricing_sdk::terms::{BillingTiming, InputSource, InvoiceInputs, Rounding};
use bss_products_sdk::models::{Lifecycle, Sku, SkuType};
use std::collections::BTreeMap;
use std::sync::Arc;
use time::{Date, Month, OffsetDateTime};
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

/// Pricing's own plan resource (`pricing_reads.rs`), mirrored: the builder is the platform's.
#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.pricing.plan.v1~"))]
struct PlanResource;
/// A different Pricing resource, to show only the plan resource means "revision absent".
#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.pricing.price.v1~"))]
struct PriceResource;

fn ids() -> (Uuid, Uuid, Uuid, Uuid) {
    (
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        Uuid::from_u128(3),
        Uuid::from_u128(4),
    )
} // plan, revision, item, sku
fn day() -> Date {
    Date::from_calendar_date(2026, Month::October, 22).unwrap()
}
fn sel(item: Uuid, dim: Option<&str>) -> SelectedItem {
    SelectedItem {
        item_id: item,
        quantity: "1".into(),
        selected_dim_value: dim.map(str::to_owned),
    }
}
fn req_items(items: Vec<SelectedItem>) -> LineCatalogRequest {
    let (plan, rev, _, _) = ids();
    LineCatalogRequest {
        line_id: Uuid::from_u128(9),
        plan_id: plan,
        revision_id: rev,
        items,
    }
}
fn req(dim: Option<&str>) -> LineCatalogRequest {
    req_items(vec![sel(ids().2, dim)])
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "the `CatalogAnswer::current` field type"
)]
fn current() -> Result<RevisionRef, ReadFailure> {
    let (plan, rev, _, _) = ids();
    Ok(RevisionRef {
        plan_id: plan,
        revision_id: rev,
        revision_no: 1,
    })
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "the `CatalogAnswer::resolved` field type"
)]
fn resolved(cells: Vec<ResolvedCell>) -> Result<Arc<ResolvedBindings>, ReadFailure> {
    let (plan, rev, _, _) = ids();
    Ok(Arc::new(ResolvedBindings {
        plan_id: plan,
        revision_id: rev,
        cells,
    }))
}
/// Every pub field of `AcceptedBinding`, `ImmutablePrice` (state `Approved`) and `InvoiceInputs`.
fn binding(sku: Uuid) -> AcceptedBinding {
    AcceptedBinding {
        item_id: ids().2,
        price_book_entry_id: Uuid::from_u128(20),
        dimension_key: Some("region".into()),
        dimension_value: Some("x".into()),
        sku_id: sku,
        sku_version: 3,
        sku_code: "VM-SMALL".into(),
        sku_name: "Small VM".into(),
        unit: Some("instance".into()),
        meter: None,
        price: ImmutablePrice {
            price_id: Uuid::from_u128(21),
            price_book_entry_id: Uuid::from_u128(20),
            money_digest: [7; 32],
            currency: bss_money::CurrencySpec::try_new("USD".into(), 2).unwrap(),
            model: PriceModel::Flat {
                amount: bss_money::parse_decimal("10").unwrap(),
            },
            minimum_fee: None,
            effective_from: Date::from_calendar_date(2026, Month::January, 1).unwrap(),
            ends_on: None,
            state: PriceState::Approved,
        },
        kind: ChargeKind::Recurring,
        recurring_period: None,
        via_default: false,
        usage_rating_policy: None,
        invoice: InvoiceInputs {
            template: "{sku_name}".into(),
            template_digest: [8; 32],
            template_source: InputSource::Entry,
            gl_code: "4000".into(),
            tax_category: "standard".into(),
            timing: BillingTiming::Advance,
            rounding: Rounding::HalfEven,
        },
    }
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "the `ResolvedCell::binding` field type"
)]
fn bound() -> Option<AcceptedBinding> {
    Some(binding(ids().3))
}
fn cell_of(item: Uuid, dim: Option<&str>, b: Option<AcceptedBinding>) -> ResolvedCell {
    ResolvedCell {
        selection: BindingSelection {
            item_id: item,
            dimension_value: dim.map(str::to_owned),
        },
        binding: b,
    }
}
fn cell(dim: Option<&str>, b: Option<AcceptedBinding>) -> ResolvedCell {
    cell_of(ids().2, dim, b)
}
fn sku(lifecycle: Lifecycle, sellable: bool) -> Sku {
    let t = OffsetDateTime::UNIX_EPOCH;
    Sku {
        id: ids().3,
        tenant_id: Uuid::from_u128(30),
        code: "VM-SMALL".into(),
        name: "Small VM".into(),
        r#type: SkuType::Recurring,
        category_id: None,
        description: "small".into(),
        sellable,
        lifecycle,
        retire_pending: false,
        lifecycle_next: None,
        revision: 1,
        published_version: 3,
        gl_code: None,
        tax_category: None,
        invoice_line_template: None,
        billing_timing: None,
        usage_type_ref: None,
        unit: None,
        type_change_pending: false,
        pending_unit_id: None,
        approved_by_unit_id: None,
        created_by: Uuid::from_u128(31),
        created_at: t,
        updated_at: t,
        archived_at: None,
        archived_by: None,
    }
}
fn skus(a: PortAnswer<Sku>) -> BTreeMap<Uuid, PortAnswer<Sku>> {
    BTreeMap::from([(ids().3, a)])
}
fn good_skus() -> BTreeMap<Uuid, PortAnswer<Sku>> {
    skus(PortAnswer::Answered(sku(Lifecycle::Published, true)))
}
fn answer(cells: Vec<ResolvedCell>) -> CatalogAnswer {
    CatalogAnswer {
        current: current(),
        resolved: resolved(cells),
        skus: good_skus(),
    }
}
fn failing(r: ReadFailure) -> CatalogAnswer {
    CatalogAnswer {
        current: current(),
        resolved: Err(r),
        skus: good_skus(),
    }
}
fn outcome(lc: &LineCatalog, p: PredicateId) -> &Outcome {
    lc.outcomes.iter().find(|o| o.key.predicate == p).unwrap()
}
fn verdict(lc: &LineCatalog, p: PredicateId) -> Verdict {
    outcome(lc, p).verdict
}
fn reason(lc: &LineCatalog, p: PredicateId) -> Option<Reason> {
    outcome(lc, p).reason
}
fn code(lc: &LineCatalog, p: PredicateId) -> Option<String> {
    outcome(lc, p)
        .upstream_detail
        .as_ref()
        .and_then(|d| d.upstream_code.clone())
}
/// The rows of one slot that share its `(item, dim)` identity.
fn slot_row(lc: &LineCatalog, p: PredicateId, item: Uuid, dim: Option<&str>) -> Outcome {
    lc.outcomes
        .iter()
        .find(|o| {
            o.key.predicate == p
                && o.key.item_id == Some(item)
                && o.key.selection.catalog_scope_key() == dim
        })
        .cloned()
        .unwrap()
}
const SLOT_DEPENDENTS: [PredicateId; 4] = [
    PredicateId::CatalogItemMembership,
    PredicateId::CatalogSlotCoverage,
    PredicateId::CatalogSkuVersionDated,
    PredicateId::CatalogSkuSellable,
];

#[test]
fn uncovered_selected_cell_fails_coverage_and_pin_is_unresolvable() {
    // Rule 7: a selected cell with `binding: None` fails coverage, never free.
    let lc = map_line(&req(Some("x")), &answer(vec![cell(Some("x"), None)]), day());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogItemMembership),
        Verdict::Passed
    );
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSlotCoverage),
        Verdict::Failed
    );
    assert_eq!(
        reason(&lc, PredicateId::CatalogSlotCoverage),
        Some(Reason::CatalogPredicateFailed)
    );
    assert!(matches!(
        lc.pin,
        PinOutcome::Unresolvable(Reason::PinUnresolvable)
    ));
    assert_uncovered_slot_keeps_every_row(&lc, Some("x"));
    // Rule 7: an unselected uncovered cell is ignored.
    let lc = map_line(
        &req(Some("x")),
        &answer(vec![cell(Some("x"), bound()), cell(Some("y"), None)]),
        day(),
    );
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSlotCoverage),
        Verdict::Passed
    );
    assert!(matches!(&lc.pin, PinOutcome::Composed { bindings, .. } if bindings.len() == 1));
    // Rule 7: a selection with no matching cell fails with `dim-value-unknown`.
    let lc = map_line(
        &req(Some("z")),
        &answer(vec![cell(Some("x"), bound())]),
        day(),
    );
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSlotCoverage),
        Verdict::Failed
    );
    assert_eq!(
        code(&lc, PredicateId::CatalogSlotCoverage).as_deref(),
        Some("dim-value-unknown")
    );
    assert!(matches!(
        lc.pin,
        PinOutcome::Unresolvable(Reason::PinUnresolvable)
    ));
    assert_uncovered_slot_keeps_every_row(&lc, Some("z"));
}

/// An uncovered or absent slot keeps predicates 5 and 6 as unevaluable rows with the coverage
/// row's identity, so the line has the 9 rows of a covered slot and the vector builds.
fn assert_uncovered_slot_keeps_every_row(lc: &LineCatalog, dim: Option<&str>) {
    let item = ids().2;
    let coverage = slot_row(lc, PredicateId::CatalogSlotCoverage, item, dim);
    for p in [
        PredicateId::CatalogSkuVersionDated,
        PredicateId::CatalogSkuSellable,
    ] {
        let row = slot_row(lc, p, item, dim);
        assert_eq!(row.verdict, Verdict::Unevaluable, "{p:?}");
        assert_eq!(
            row.reason,
            Some(Reason::CatalogPredicateUnevaluable),
            "{p:?}"
        );
        assert_eq!(
            (row.key.line_id, row.key.item_id, &row.key.selection,),
            (
                coverage.key.line_id,
                coverage.key.item_id,
                &coverage.key.selection,
            ),
            "{p:?}"
        );
    }
    assert_eq!(lc.outcomes.len(), 9);
    assert!(Vector::build(lc.outcomes.clone()).is_ok());
}

#[test]
fn selected_null_on_dimensioned_item_passes_only_via_bound_default() {
    // Pricing sets `via_default` only for a dimension value that fell back to the default chain,
    // so the NULL cell's binding always has `via_default == false` (`api/pricing_read.rs`).
    let null_binding = || {
        let mut b = binding(ids().3);
        b.dimension_value = None;
        b.via_default = false;
        Some(b)
    };
    // Bound default slot (the NULL cell) of a dimensioned item: passes, pin composes with it.
    let lc = map_line(
        &req(None),
        &answer(vec![cell(Some("x"), bound()), cell(None, null_binding())]),
        day(),
    );
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSlotCoverage),
        Verdict::Passed
    );
    let key = &outcome(&lc, PredicateId::CatalogSlotCoverage).key;
    assert_eq!(key.selection, CatalogSelection::SelectedNull);
    match &lc.pin {
        PinOutcome::Composed { bindings, revision } => {
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].dimension_value, None);
            assert!(!bindings[0].via_default);
            assert_eq!(revision.revision_id, ids().1);
        }
        other => panic!("expected a composed pin, got {other:?}"),
    }
    // Unbound default slot: fails.
    let lc = map_line(
        &req(None),
        &answer(vec![cell(Some("x"), bound()), cell(None, None)]),
        day(),
    );
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSlotCoverage),
        Verdict::Failed
    );
    assert!(matches!(
        lc.pin,
        PinOutcome::Unresolvable(Reason::PinUnresolvable)
    ));
    // No default slot in the matrix: fails with `dim-value-unknown`.
    let lc = map_line(&req(None), &answer(vec![cell(Some("x"), bound())]), day());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSlotCoverage),
        Verdict::Failed
    );
    assert_eq!(
        code(&lc, PredicateId::CatalogSlotCoverage).as_deref(),
        Some("dim-value-unknown")
    );
    // A dimensionless item: its NULL slot's binding passes.
    let lc = map_line(&req(None), &answer(vec![cell(None, null_binding())]), day());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSlotCoverage),
        Verdict::Passed
    );
}

#[test]
fn current_mismatch_fails_predicate_one_without_substitution() {
    let (plan, _, _, _) = ids();
    let other = Uuid::from_u128(7);
    let ans = CatalogAnswer {
        current: Ok(RevisionRef {
            plan_id: plan,
            revision_id: other,
            revision_no: 2,
        }),
        resolved: resolved(vec![cell(Some("x"), bound())]),
        skus: good_skus(),
    };
    let lc = map_line(&req(Some("x")), &ans, day());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogRevisionCurrent),
        Verdict::Failed
    );
    assert_eq!(
        reason(&lc, PredicateId::CatalogRevisionCurrent),
        Some(Reason::CatalogPredicateFailed)
    );
    let detail = outcome(&lc, PredicateId::CatalogRevisionCurrent)
        .upstream_detail
        .clone()
        .unwrap();
    assert_eq!(detail.current_revision_id, Some(other));
    assert_eq!(detail.revision_id, Some(ids().1));
    // Never substituted: no pin on either revision; the slot predicates still evaluate.
    assert!(!matches!(lc.pin, PinOutcome::Composed { .. }));
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSlotCoverage),
        Verdict::Passed
    );
}

#[test]
fn revision_absent_is_failed_absent_and_dependents_are_unevaluable_with_the_same_reason() {
    let lc = map_line(
        &req(Some("x")),
        &failing(ReadFailure::RevisionAbsent),
        day(),
    );
    assert_eq!(
        verdict(&lc, PredicateId::CatalogRevisionCurrent),
        Verdict::Failed
    );
    assert_eq!(
        reason(&lc, PredicateId::CatalogRevisionCurrent),
        Some(Reason::PricingRevisionAbsent)
    );
    for p in SLOT_DEPENDENTS {
        assert_eq!(verdict(&lc, p), Verdict::Unevaluable, "{p:?}");
        assert_eq!(reason(&lc, p), Some(Reason::PricingRevisionAbsent), "{p:?}");
    }
    assert!(matches!(
        lc.pin,
        PinOutcome::Unevaluable(Reason::PricingRevisionAbsent)
    ));
    // Every row is kept and the vector builds.
    assert!(Vector::build(lc.outcomes).is_ok());
}

#[test]
fn not_published_codes_fail_predicate_one_and_keep_the_code_as_upstream_detail() {
    for (failure, token) in [
        (ReadFailure::RevisionNotPublished, "REVISION_NOT_PUBLISHED"),
        (
            ReadFailure::RevisionNotYetAvailable,
            "REVISION_NOT_YET_AVAILABLE",
        ),
    ] {
        let lc = map_line(&req(Some("x")), &failing(failure), day());
        assert_eq!(
            verdict(&lc, PredicateId::CatalogRevisionCurrent),
            Verdict::Failed
        );
        assert_eq!(
            reason(&lc, PredicateId::CatalogRevisionCurrent),
            Some(Reason::CatalogPredicateFailed)
        );
        assert_eq!(
            code(&lc, PredicateId::CatalogRevisionCurrent).as_deref(),
            Some(token)
        );
        for p in SLOT_DEPENDENTS {
            assert_eq!(verdict(&lc, p), Verdict::Unevaluable, "{token} {p:?}");
            // Resolve answered: unevaluable, not unavailable.
            assert_eq!(reason(&lc, p), Some(Reason::CatalogPredicateUnevaluable));
        }
        assert!(matches!(
            lc.pin,
            PinOutcome::Unevaluable(Reason::CatalogPinCompositionUnavailable)
        ));
    }
}

#[test]
fn incomplete_inputs_never_pass() {
    let lc = map_line(
        &req(Some("x")),
        &failing(ReadFailure::IncompleteCommercialInputs("gl_code".into())),
        day(),
    );
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSlotCoverage),
        Verdict::Unevaluable
    );
    for p in SLOT_DEPENDENTS {
        assert_eq!(verdict(&lc, p), Verdict::Unevaluable, "{p:?}");
        // Resolve answered: unevaluable, not unavailable.
        assert_eq!(
            reason(&lc, p),
            Some(Reason::CatalogPredicateUnevaluable),
            "{p:?}"
        );
    }
    assert_ne!(
        verdict(&lc, PredicateId::ReferenceResolution),
        Verdict::Passed
    );
    assert!(matches!(
        lc.pin,
        PinOutcome::Unevaluable(Reason::CatalogPinCompositionUnavailable)
    ));
}

#[test]
fn resolve_outage_makes_adopted_and_pin_unevaluable_but_current_still_answers() {
    // Rule 6: resolve unavailable; the current revision still answers predicate 1.
    let lc = map_line(
        &req(Some("x")),
        &failing(ReadFailure::Unavailable(Unavailable::Outage)),
        day(),
    );
    assert_eq!(
        verdict(&lc, PredicateId::CatalogRevisionCurrent),
        Verdict::Passed
    );
    for p in SLOT_DEPENDENTS
        .into_iter()
        .chain([PredicateId::ReferenceResolution])
    {
        assert_eq!(verdict(&lc, p), Verdict::Unevaluable, "{p:?}");
        assert_eq!(reason(&lc, p), Some(Reason::CatalogPredicatesUnavailable));
    }
    assert!(matches!(
        lc.pin,
        PinOutcome::Unevaluable(Reason::CatalogPinCompositionUnavailable)
    ));
    // Rule 1: current unavailable; resolve answered, so 3–6 keep their own results.
    let ans = CatalogAnswer {
        current: Err(ReadFailure::Unavailable(Unavailable::Deadline)),
        resolved: resolved(vec![cell(Some("x"), bound())]),
        skus: good_skus(),
    };
    let lc = map_line(&req(Some("x")), &ans, day());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogRevisionCurrent),
        Verdict::Unevaluable
    );
    assert_eq!(
        reason(&lc, PredicateId::CatalogRevisionCurrent),
        Some(Reason::PricingRevisionUnavailable)
    );
    for p in SLOT_DEPENDENTS {
        assert_eq!(verdict(&lc, p), Verdict::Passed, "{p:?}");
    }
    assert!(matches!(
        lc.pin,
        PinOutcome::Unevaluable(Reason::PricingRevisionUnavailable)
    ));
}

#[test]
fn cancelled_price_fails_dated_sku_predicate() {
    let with = |f: &dyn Fn(&mut AcceptedBinding)| {
        let mut b = binding(ids().3);
        f(&mut b);
        map_line(
            &req(Some("x")),
            &answer(vec![cell(Some("x"), Some(b))]),
            day(),
        )
    };
    let lc = with(&|_| {});
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSkuVersionDated),
        Verdict::Passed
    );
    let lc = with(&|b| b.price.state = PriceState::Cancelled);
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSkuVersionDated),
        Verdict::Failed
    );
    assert_eq!(
        reason(&lc, PredicateId::CatalogSkuVersionDated),
        Some(Reason::CatalogPredicateFailed)
    );
    let detail = outcome(&lc, PredicateId::CatalogSkuVersionDated)
        .upstream_detail
        .clone()
        .unwrap();
    assert_eq!(
        (detail.sku_id, detail.sku_version),
        (Some(ids().3), Some(3))
    );
    // Outside the dated window: not yet in force, or ended before the date.
    let lc = with(&|b| b.price.effective_from = day().next_day().unwrap());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSkuVersionDated),
        Verdict::Failed
    );
    let lc = with(&|b| b.price.ends_on = day().previous_day());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSkuVersionDated),
        Verdict::Failed
    );
    // `ends_on` is exclusive, as Pricing's `[effective_from, end)` (`domain/price.rs`).
    let lc = with(&|b| b.price.ends_on = Some(day()));
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSkuVersionDated),
        Verdict::Failed
    );
    let lc = with(&|b| b.price.ends_on = day().next_day());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSkuVersionDated),
        Verdict::Passed
    );
    let lc = with(&|b| b.price.effective_from = day());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogSkuVersionDated),
        Verdict::Passed
    );
}

#[test]
fn sku_deprecated_vs_retired_vs_unavailable() {
    let run = |s: BTreeMap<Uuid, PortAnswer<Sku>>| {
        let ans = CatalogAnswer {
            current: current(),
            resolved: resolved(vec![cell(Some("x"), bound())]),
            skus: s,
        };
        map_line(&req(Some("x")), &ans, day())
    };
    let p = PredicateId::CatalogSkuSellable;
    let lc = run(good_skus());
    assert_eq!(verdict(&lc, p), Verdict::Passed);
    for (lifecycle, sellable, token) in [
        (Lifecycle::Deprecated, true, "sku-deprecated"),
        (Lifecycle::Retired, true, "sku-retired"),
        (Lifecycle::Draft, true, "sku-draft"),
        (Lifecycle::Published, false, "sku-not-sellable"),
    ] {
        let lc = run(skus(PortAnswer::Answered(sku(lifecycle, sellable))));
        assert_eq!(verdict(&lc, p), Verdict::Failed, "{token}");
        assert_eq!(reason(&lc, p), Some(Reason::CatalogPredicateFailed));
        assert_eq!(code(&lc, p).as_deref(), Some(token));
    }
    let lc = run(skus(PortAnswer::Unavailable(Unavailable::Outage)));
    assert_eq!(verdict(&lc, p), Verdict::Unevaluable);
    assert_eq!(reason(&lc, p), Some(Reason::CatalogPredicatesUnavailable));
    // No answer at all for the bound SKU is never a pass.
    let lc = run(BTreeMap::new());
    assert_eq!(verdict(&lc, p), Verdict::Unevaluable);
    assert_eq!(reason(&lc, p), Some(Reason::CatalogPredicatesUnavailable));
    // An answered SKU of another id cannot evaluate the predicate.
    let mut other = sku(Lifecycle::Published, true);
    other.id = Uuid::from_u128(6);
    let lc = run(skus(PortAnswer::Answered(other)));
    assert_eq!(verdict(&lc, p), Verdict::Unevaluable);
    assert_eq!(reason(&lc, p), Some(Reason::CatalogPredicateUnevaluable));
}

#[test]
fn plan_unpublished_from_current_revision_fails_predicate_one_with_the_code() {
    // Only `current_revision` raises PLAN_UNPUBLISHED; resolve still answers the dependents.
    let ans = CatalogAnswer {
        current: Err(ReadFailure::PlanUnpublished),
        resolved: resolved(vec![cell(Some("x"), bound())]),
        skus: good_skus(),
    };
    let lc = map_line(&req(Some("x")), &ans, day());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogRevisionCurrent),
        Verdict::Failed
    );
    assert_eq!(
        reason(&lc, PredicateId::CatalogRevisionCurrent),
        Some(Reason::CatalogPredicateFailed)
    );
    let detail = outcome(&lc, PredicateId::CatalogRevisionCurrent)
        .upstream_detail
        .clone()
        .unwrap();
    assert_eq!(detail.upstream_code.as_deref(), Some("PLAN_UNPUBLISHED"));
    assert_eq!(
        (detail.plan_id, detail.revision_id),
        (Some(ids().0), Some(ids().1))
    );
    for p in SLOT_DEPENDENTS
        .into_iter()
        .chain([PredicateId::ReferenceResolution])
    {
        assert_eq!(verdict(&lc, p), Verdict::Passed, "{p:?}");
    }
    // The pin carries predicate 1's reason, as on a current mismatch.
    assert!(matches!(
        lc.pin,
        PinOutcome::Unevaluable(Reason::CatalogPredicateFailed)
    ));
    assert!(Vector::build(lc.outcomes).is_ok());
}

#[test]
fn current_unavailable_outranks_an_answered_absent_revision() {
    let ans = CatalogAnswer {
        current: Err(ReadFailure::Unavailable(Unavailable::Outage)),
        resolved: Err(ReadFailure::RevisionAbsent),
        skus: good_skus(),
    };
    let lc = map_line(&req(Some("x")), &ans, day());
    assert_eq!(
        verdict(&lc, PredicateId::CatalogRevisionCurrent),
        Verdict::Unevaluable
    );
    assert_eq!(
        reason(&lc, PredicateId::CatalogRevisionCurrent),
        Some(Reason::PricingRevisionUnavailable)
    );
    for p in SLOT_DEPENDENTS
        .into_iter()
        .chain([PredicateId::ReferenceResolution])
    {
        assert_eq!(verdict(&lc, p), Verdict::Unevaluable, "{p:?}");
        assert_eq!(reason(&lc, p), Some(Reason::PricingRevisionAbsent), "{p:?}");
    }
    assert!(matches!(
        lc.pin,
        PinOutcome::Unevaluable(Reason::PricingRevisionUnavailable)
    ));
}

#[test]
fn predicates_two_seven_eight_are_unevaluable_until_d189() {
    let lc = map_line(
        &req(Some("x")),
        &answer(vec![cell(Some("x"), bound())]),
        day(),
    );
    for p in [
        PredicateId::CatalogRevisionAvailable,
        PredicateId::CatalogDimensionMarket,
        PredicateId::CatalogEligibilityRules,
    ] {
        assert_eq!(verdict(&lc, p), Verdict::Unevaluable, "{p:?}");
        assert_eq!(reason(&lc, p), Some(Reason::CatalogPredicatesUnavailable));
    }
    // Everything else answered passes; the pin composes; the rows build a valid vector.
    for p in [
        PredicateId::CatalogRevisionCurrent,
        PredicateId::CatalogItemMembership,
        PredicateId::CatalogSlotCoverage,
        PredicateId::CatalogSkuVersionDated,
        PredicateId::CatalogSkuSellable,
        PredicateId::ReferenceResolution,
    ] {
        assert_eq!(verdict(&lc, p), Verdict::Passed, "{p:?}");
    }
    assert!(matches!(lc.pin, PinOutcome::Composed { .. }));
    assert_eq!(lc.outcomes.len(), 9);
    assert!(Vector::build(lc.outcomes).is_ok());
}

#[test]
fn duplicate_selection_and_foreign_item_are_reference_failures_and_pin_carries_the_same_reason_once()
 {
    let item = ids().2;
    // Duplicate `(item, dim)`: reference-duplicated; the slot is evaluated once (unique rows).
    let lc = map_line(
        &req_items(vec![sel(item, Some("x")), sel(item, Some("x"))]),
        &answer(vec![cell(Some("x"), bound())]),
        day(),
    );
    assert_eq!(
        verdict(&lc, PredicateId::ReferenceResolution),
        Verdict::Failed
    );
    assert_eq!(
        reason(&lc, PredicateId::ReferenceResolution),
        Some(Reason::ReferenceDuplicated)
    );
    assert!(matches!(
        lc.pin,
        PinOutcome::Unevaluable(Reason::ReferenceDuplicated)
    ));
    let coverage_rows = lc
        .outcomes
        .iter()
        .filter(|o| o.key.predicate == PredicateId::CatalogSlotCoverage)
        .count();
    assert_eq!(coverage_rows, 1);
    assert!(Vector::build(lc.outcomes).is_ok());
    // Same item, different dimension values: two slots, no duplicate.
    let lc = map_line(
        &req_items(vec![sel(item, Some("x")), sel(item, Some("y"))]),
        &answer(vec![cell(Some("x"), bound()), cell(Some("y"), bound())]),
        day(),
    );
    assert_eq!(
        verdict(&lc, PredicateId::ReferenceResolution),
        Verdict::Passed
    );
    assert!(matches!(&lc.pin, PinOutcome::Composed { bindings, .. } if bindings.len() == 2));
    // An item outside the revision's roster: reference-unresolvable, membership fails.
    let foreign = Uuid::from_u128(5);
    let lc = map_line(
        &req_items(vec![sel(item, Some("x")), sel(foreign, None)]),
        &answer(vec![cell(Some("x"), bound())]),
        day(),
    );
    assert_eq!(
        verdict(&lc, PredicateId::ReferenceResolution),
        Verdict::Failed
    );
    assert_eq!(
        reason(&lc, PredicateId::ReferenceResolution),
        Some(Reason::ReferenceUnresolvable)
    );
    let foreign_membership = lc
        .outcomes
        .iter()
        .find(|o| {
            o.key.predicate == PredicateId::CatalogItemMembership && o.key.item_id == Some(foreign)
        })
        .unwrap();
    assert_eq!(foreign_membership.verdict, Verdict::Failed);
    let detail = foreign_membership.upstream_detail.clone().unwrap();
    assert_eq!(
        (detail.plan_id, detail.revision_id),
        (Some(ids().0), Some(ids().1))
    );
    assert!(matches!(
        lc.pin,
        PinOutcome::Unevaluable(Reason::ReferenceUnresolvable)
    ));
    // The reference outcome is line-scoped.
    let key = &outcome(&lc, PredicateId::ReferenceResolution).key;
    assert!(key.item_id.is_none() && key.selection == CatalogSelection::NotSelected);
}

#[test]
fn classify_read_error_table() {
    let absent = PlanResource::not_found("plan_revision not found")
        .with_resource("plan_revision")
        .create();
    assert_eq!(classify_read_error(&absent), ReadFailure::RevisionAbsent);
    // An item-level not-found is not "no such revision": fail closed as unavailable.
    let item = PlanResource::not_found("plan_item not found")
        .with_resource("plan_item")
        .create();
    assert_eq!(
        classify_read_error(&item),
        ReadFailure::Unavailable(Unavailable::Outage)
    );
    let price = PriceResource::not_found("price not found")
        .with_resource("price")
        .create();
    assert_eq!(
        classify_read_error(&price),
        ReadFailure::Unavailable(Unavailable::Outage)
    );
    let aborted = |c: &str| PlanResource::aborted(c).with_reason(c).create();
    for (c, want) in [
        ("REVISION_NOT_PUBLISHED", ReadFailure::RevisionNotPublished),
        (
            "REVISION_NOT_YET_AVAILABLE",
            ReadFailure::RevisionNotYetAvailable,
        ),
        ("PLAN_UNPUBLISHED", ReadFailure::PlanUnpublished),
        (
            "SERIALIZATION_FAILURE",
            ReadFailure::Unavailable(Unavailable::Outage),
        ),
    ] {
        assert_eq!(classify_read_error(&aborted(c)), want, "{c}");
    }
    let incomplete: CanonicalError = IncompleteCommercialInputs { field: "gl_code" }.into();
    assert_eq!(
        classify_read_error(&incomplete),
        ReadFailure::IncompleteCommercialInputs("gl_code".into())
    );
    let other_precondition = PlanResource::failed_precondition()
        .with_precondition_violation("x", "other", "OTHER")
        .create();
    assert_eq!(
        classify_read_error(&other_precondition),
        ReadFailure::Unavailable(Unavailable::Outage)
    );
    let denied = PlanResource::permission_denied()
        .with_reason("denied")
        .create();
    assert_eq!(
        classify_read_error(&denied),
        ReadFailure::Unavailable(Unavailable::Denied)
    );
    let slow = PlanResource::deadline_exceeded("slow").create();
    assert_eq!(
        classify_read_error(&slow),
        ReadFailure::Unavailable(Unavailable::Deadline)
    );
    let malformed = PlanResource::invalid_argument()
        .with_field_violation("pins", "bad pin", "PIN_INVALID")
        .create();
    assert_eq!(
        classify_read_error(&malformed),
        ReadFailure::Unavailable(Unavailable::Malformed)
    );
    for e in [
        CanonicalError::internal("boom").create(),
        CanonicalError::service_unavailable().create(),
    ] {
        assert_eq!(
            classify_read_error(&e),
            ReadFailure::Unavailable(Unavailable::Outage)
        );
    }
}
