//! Preview SDK types (S3-11, M2-1): wire names, round trips and request validation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]
use bss_orders_sdk::authoring::{
    AddLine, AuthoredTerm, AuthoringInvalid, BillingCycle, CalendarDate, Currency, Quantity,
    SelectedItem,
};
use bss_orders_sdk::gate_report::GateReport;
use bss_orders_sdk::preview::{
    DiscountView, ExactMoneyView, IndicativeTaxView, LineTax, MAX_PREVIEW_LINES, MoneyView,
    PinOutcomeView, PreviewBasket, PreviewLine, PreviewLineResult, PreviewResult,
    ResolvedTotalView, TcvWithheld,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use time::{Date, Month, OffsetDateTime};
use uuid::Uuid;

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn line() -> PreviewLine {
    PreviewLine {
        plan_id: id(1),
        plan_revision_id: id(2),
        selected_items: vec![SelectedItem {
            item_id: id(3),
            quantity: Some(Quantity::new("2.5".parse().unwrap()).unwrap()),
            selected_dim_value: Some("eu".into()),
        }],
        currency: Currency::try_from("EUR".to_owned()).unwrap(),
        contract_effective_date: Some(CalendarDate(
            Date::from_calendar_date(2026, Month::October, 1).unwrap(),
        )),
        service_activation_date: None,
        acceptance_due_date: None,
        term_duration: Some(AuthoredTerm::Rolling),
        billing_cycle: Some(BillingCycle::Month),
    }
}

fn basket(lines: usize) -> PreviewBasket {
    PreviewBasket {
        resource_tenant_id: id(10),
        payer_tenant_id: id(11),
        seller_tenant_id: id(12),
        contract_id: Some(id(13)),
        lines: vec![line(); lines],
    }
}

fn money(text: &str) -> MoneyView {
    MoneyView::new(ExactMoneyView::new("1250", "100", "EUR", 2), text)
}

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let wire = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(&back, value);
    assert_eq!(serde_json::to_value(&back).unwrap(), wire);
}

fn result(withheld: bool) -> PreviewResult {
    let gate = GateReport::new(id(20), "v1", vec![], vec![]);
    let lines = vec![
        PreviewLineResult::new(id(30), PinOutcomeView::Composed, None),
        PreviewLineResult::new(
            id(31),
            PinOutcomeView::Unresolvable {
                reason: "plan-revision-unknown".into(),
            },
            Some(CalendarDate(
                Date::from_calendar_date(2026, Month::November, 1).unwrap(),
            )),
        ),
        PreviewLineResult::new(
            id(32),
            PinOutcomeView::Unevaluable {
                reason: "rating-unavailable".into(),
            },
            None,
        ),
    ];
    let total = ResolvedTotalView::new(
        "EUR",
        2,
        money("12.50 EUR"),
        money("10.00 EUR"),
        DiscountView::Available(money("2.50 EUR")),
        true,
    );
    let tax = IndicativeTaxView::new(
        vec![LineTax::new(id(30), money("1.00 EUR"))],
        money("1.00 EUR"),
        false,
    );
    let mut r = PreviewResult::new(gate, lines);
    r.resolved_total = Some(total);
    r.indicative_tax = Some(tax);
    r.expected_fulfillment_time = Some(OffsetDateTime::UNIX_EPOCH);
    if withheld {
        r.tcv_withheld = Some(TcvWithheld::new(
            "preview-term-or-cycle-missing",
            vec![id(30), id(31)],
        ));
    } else {
        r.tcv = Some(money("120.00 EUR"));
    }
    r
}

#[test]
fn every_type_round_trips() {
    round_trip(&basket(2));
    round_trip(&line());
    round_trip(&result(false));
    round_trip(&result(true));
    round_trip(&PreviewLineResult::new(
        id(1),
        PinOutcomeView::Composed,
        None,
    ));
    round_trip(&TcvWithheld::new("r", vec![id(1)]));
    round_trip(&money("1 EUR"));
    round_trip(&ExactMoneyView::new("-5", "3", "EUR", 2));
    round_trip(&DiscountView::Unavailable);
    round_trip(&DiscountView::Available(money("1 EUR")));
    round_trip(&IndicativeTaxView::new(vec![], money("0 EUR"), true));
}

#[test]
fn tcv_withheld_wire_names_are_pinned() {
    let wire = serde_json::to_value(result(true)).unwrap();
    assert!(wire.get("tcvWithheld").is_some());
    assert!(wire.get("tcv_withheld").is_none());
    assert!(wire.get("tcv").is_none());
    assert_eq!(
        wire["tcvWithheld"]["lineIds"],
        json!([id(30).to_string(), id(31).to_string()])
    );
    assert!(wire["tcvWithheld"].get("line_ids").is_none());
    assert!(wire.get("assessment_id").is_some());
    assert!(wire.get("expected_fulfillment_time").is_some());
    let present = serde_json::to_value(result(false)).unwrap();
    assert!(present.get("tcv").is_some());
    assert!(present.get("tcvWithheld").is_none());
}

#[test]
fn money_is_text_verbatim() {
    let wire = serde_json::to_value(money("12.50 EUR")).unwrap();
    assert_eq!(wire["exact"]["numerator"], "1250");
    assert_eq!(wire["exact"]["denominator"], "100");
    assert_eq!(wire["exact"]["currency_scale"], 2);
    assert_eq!(wire["display"], "12.50 EUR");
}

#[test]
fn requests_refuse_unknown_fields_results_tolerate_them() {
    let mut wire = serde_json::to_value(basket(1)).unwrap();
    wire["extra"] = json!(1);
    assert!(serde_json::from_value::<PreviewBasket>(wire).is_err());
    let mut wire = serde_json::to_value(line()).unwrap();
    wire["extra"] = json!(1);
    assert!(serde_json::from_value::<PreviewLine>(wire).is_err());
    let mut wire = serde_json::to_value(result(false)).unwrap();
    wire["extra"] = json!(1);
    assert!(serde_json::from_value::<PreviewResult>(wire).is_ok());
}

#[test]
fn line_limit_is_two_hundred() {
    assert_eq!(MAX_PREVIEW_LINES, 200);
    basket(200).validate().unwrap();
    assert_eq!(basket(201).validate(), Err(AuthoringInvalid("lines")));
}

#[test]
fn a_line_failing_add_line_rules_fails_the_basket() {
    let mut bad = line();
    bad.term_duration = Some(AuthoredTerm::Periods { count: 0 });
    assert_eq!(bad.validate(), Err(AuthoringInvalid("term_duration")));
    let mut b = basket(2);
    b.lines[1] = bad;
    assert_eq!(b.validate(), Err(AuthoringInvalid("term_duration")));
}

#[test]
fn per_line_tax_is_a_named_object() {
    let wire = serde_json::to_value(result(false)).unwrap();
    let entry = &wire["indicative_tax"]["per_line"][0];
    assert!(entry.is_object());
    assert_eq!(entry["line_id"], id(30).to_string());
    assert_eq!(entry["tax"]["display"], "1.00 EUR");
}

#[test]
fn result_assessment_id_is_the_gate_reports() {
    let r = result(false);
    assert_eq!(r.assessment_id, r.assessment.assessment_id);
    assert_eq!(r.assessment_id, id(20));
}

#[test]
fn preview_result_refuses_disagreeing_assessment_ids() {
    let mut wire = serde_json::to_value(result(false)).unwrap();
    wire["assessment_id"] = json!(id(99));
    assert!(serde_json::from_value::<PreviewResult>(wire).is_err());
    // The agreeing body still decodes, and unknown members stay tolerated.
    let mut wire = serde_json::to_value(result(false)).unwrap();
    wire["extra"] = json!(1);
    assert_eq!(
        serde_json::from_value::<PreviewResult>(wire).unwrap(),
        result(false)
    );
}

#[test]
fn preview_line_validation_matches_add_line() {
    fn item(dim: Option<&str>) -> SelectedItem {
        SelectedItem {
            item_id: id(3),
            quantity: None,
            selected_dim_value: dim.map(str::to_owned),
        }
    }
    fn unencodable() -> CalendarDate {
        CalendarDate(Date::MIN)
    }
    type Edit = fn(&mut PreviewLine);
    let table: Vec<(&str, Edit, Option<&str>)> = vec![
        ("valid", |_| {}, None),
        (
            "too many items",
            |l| l.selected_items = vec![item(None); 1_001],
            Some("selected_items"),
        ),
        (
            "empty dimension value",
            |l| l.selected_items = vec![item(Some(""))],
            Some("selected_dim_value"),
        ),
        (
            "zero periods",
            |l| l.term_duration = Some(AuthoredTerm::Periods { count: 0 }),
            Some("term_duration"),
        ),
        (
            "unencodable effective date",
            |l| l.contract_effective_date = Some(unencodable()),
            Some("contract_effective_date"),
        ),
        (
            "unencodable activation date",
            |l| l.service_activation_date = Some(unencodable()),
            Some("service_activation_date"),
        ),
        (
            "unencodable acceptance date",
            |l| l.acceptance_due_date = Some(unencodable()),
            Some("acceptance_due_date"),
        ),
    ];
    for (name, edit, expected) in table {
        let mut preview = line();
        edit(&mut preview);
        let add = AddLine::from(preview.clone());
        let expected = expected.map(AuthoringInvalid).map_or(Ok(()), Err);
        assert_eq!(preview.validate(), expected, "{name}");
        assert_eq!(add.validate(), expected, "{name} (AddLine)");
    }
}

#[test]
fn unknown_enum_kinds_decode_to_unknown() {
    let pin: PinOutcomeView = serde_json::from_value(json!({"kind": "something-new"})).unwrap();
    assert_eq!(pin, PinOutcomeView::Unknown);
    let discount: DiscountView = serde_json::from_value(json!({"kind": "something-new"})).unwrap();
    assert_eq!(discount, DiscountView::Unknown);
}

#[test]
fn an_empty_basket_is_refused() {
    assert_eq!(basket(0).validate(), Err(AuthoringInvalid("lines")));
}

#[test]
fn additive_preview_metadata_survives_decode_and_reencode() {
    let mut wire = serde_json::to_value(result(false)).unwrap();
    wire["tcv_basis"] = json!("mixed");
    wire["resolved_total"]["exclusions"] = json!({"tax": true, "brand": ["subscription-context"]});
    let decoded: PreviewResult = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
}

#[test]
fn successful_transition_assessment_survives_decode_and_reencode() {
    let wire =
        json!({"order_id": id(1), "state": "submitted", "version": 2, "assessment_id": id(20)});
    let decoded: bss_orders_sdk::models::TransitionResult =
        serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
}

#[test]
fn older_transition_without_an_assessment_remains_unchanged() {
    let wire = json!({"order_id": id(1), "state": "draft", "version": 1});
    let decoded: bss_orders_sdk::models::TransitionResult =
        serde_json::from_value(wire.clone()).unwrap();
    assert!(decoded.assessment_id.is_none());
    assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
}
