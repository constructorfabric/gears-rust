//! The committed read path over plain rows (lib tier): one valid submitted order and line built
//! from the SDK's golden schema-2 pin, then single-field corruptions of the independently
//! selected row, each of which must refuse as `Integration` without composing a line.
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::authoring::DraftLine;
use bss_orders_sdk::catalog::OrderState;
use serde_json::{Value, json};
use time::macros::date;
use uuid::Uuid;

use super::{committed_line, committed_total, compose_committed_lines, validate_totals};
use crate::infra::storage::entity::{order, order_line, order_line_identity, resolved_total};

/// The SDK's golden schema-2 pin (`orders-sdk/tests/fixtures`), the commercial conformance
/// vector every codec test decodes.
const PIN: &str = include_str!("../../../orders-sdk/tests/fixtures/commercial-pin-v2.json");
const REQUEST_DIGEST: &str = "fa43630bc27ba50e07dfd4b93f140bed30e68c064e63f028fdd7440de4bb5d40";
const TERMS_DIGEST: &str = "5ecffe5acffb16099d45ffa10c41f8fffd4ea7b68be820bd0e8bb50c3e5dde1f";
const QUANTITY: &str = "123456789.1234567890123456789";

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn parent() -> order::Model {
    order::Model {
        order_id: id(0xb),
        order_number: "ORD-1".to_owned(),
        category: "new_sale".to_owned(),
        resource_tenant_id: id(0xa),
        audit_tenant_id: id(0xa),
        payer_tenant_id: id(0x9),
        seller_tenant_id: id(0x8),
        initiating_actor: "user".to_owned(),
        sales_path: "self_service".to_owned(),
        contract_id: None,
        state: OrderState::Submitted.token().to_owned(),
        state_entered_at: time::OffsetDateTime::UNIX_EPOCH,
        current_version: 2,
        version_allocation_high_water: 2,
        draft_revision: 1,
        pre_hold_state: None,
        resume_count: 0,
        amendment_count: 0,
        fulfillment_control_generation: 0,
        fulfillment_control_pending: None,
        spawn_signal_at: None,
        authorization_failure_tolerated_at: None,
        compensation_evidence: None,
        audit_sequence: 0,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

fn row() -> order_line::Model {
    order_line::Model {
        order_id: id(0xb),
        version: 2,
        line_id: id(0xc),
        commercial_attempt_id: Some(id(0xa77)),
        pricing_acceptance_id: Some(id(0x14)),
        pricing_request_digest: Some(REQUEST_DIGEST.to_owned()),
        pricing_terms_digest: Some(TERMS_DIGEST.to_owned()),
        plan_id: id(0x6),
        plan_revision_id: id(0x7),
        selected_items: json!([
            {"item_id": id(0x1), "quantity": QUANTITY, "selected_dim_value": null},
            {"item_id": id(0x2), "quantity": QUANTITY, "selected_dim_value": "default"},
            {"item_id": id(0x3), "quantity": QUANTITY, "selected_dim_value": "eu"},
        ]),
        currency: "EUR".to_owned(),
        contract_effective_date: date!(2026 - 10 - 01),
        service_activation_date: None,
        acceptance_due_date: None,
        term_duration: None,
        term_kind: "missing".to_owned(),
        authored_term: Value::Null,
        billing_cycle: "month".to_owned(),
        order_pin: Some(serde_json::from_str(PIN).expect("the golden pin is JSON")),
        overlap_scope_key: None,
        overlap_item_keys: None,
        catalog_subscription_product_key: None,
        date_policy_switch_state: json!({}),
    }
}

fn total(scope: &str, line_id: Uuid, charge_kind: &str) -> resolved_total::Model {
    resolved_total::Model {
        order_id: id(0xb),
        version: 2,
        scope: scope.to_owned(),
        line_id,
        currency: "EUR".to_owned(),
        assessment_id: id(0x1e),
        currency_scale: 2,
        exclusions: json!([]),
        item_breakdown: json!([]),
        recurring_by_cycle: json!([]),
        amount_status: "committed".to_owned(),
        amount_basis: "contracted".to_owned(),
        period_evidence: json!({}),
        gross_exact: Some("1".to_owned()),
        gross_display: Some("1".to_owned()),
        net_exact: Some("1".to_owned()),
        net_display: Some("1".to_owned()),
        discount_status: "none".to_owned(),
        discount_exact: None,
        discount_display: None,
        promotion_ref: None,
        charge_kind: charge_kind.to_owned(),
        tcv_exact: None,
        tcv_display: None,
        tcv_basis: None,
        tcv_evidence: None,
    }
}

fn totals() -> Vec<resolved_total::Model> {
    vec![
        total("line", id(0xc), "recurring"),
        total("order", Uuid::nil(), "recurring"),
    ]
}

fn identity(line_id: Uuid) -> order_line_identity::Model {
    order_line_identity::Model {
        order_id: id(0xb),
        line_id,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn a_consistent_committed_line_composes_with_its_pin() {
    let line = committed_line(&parent(), &row()).expect("the golden pair composes");
    assert_eq!(line.line_id, id(0xc));
    assert_eq!(line.selected_items.len(), 3);
    let committed = line.committed.expect("a committed line carries its pin");
    assert_eq!(committed.commercial_attempt_id, id(0xa77));
    assert_eq!(committed.order_pin.assessment_id(), id(0x1e));
    validate_totals(&[line_of(&row())], &totals()).expect("the golden totals validate");
}

fn line_of(row: &order_line::Model) -> DraftLine {
    committed_line(&parent(), row).expect("the golden pair composes")
}

type Corruption = fn(&mut order::Model, &mut order_line::Model);

/// One change per independently selected fact; every one refuses before composing.
const CORRUPTIONS: [(&str, Corruption); 14] = [
    ("seller", |o, _| o.seller_tenant_id = id(0xf8)),
    ("payer", |o, _| o.payer_tenant_id = id(0xf9)),
    ("resource", |o, _| o.resource_tenant_id = id(0xfa)),
    ("order", |_, r| r.order_id = id(0xfb)),
    ("line", |_, r| r.line_id = id(0xfc)),
    ("version", |_, r| r.version = 3),
    ("plan", |_, r| r.plan_id = id(0xf6)),
    ("revision", |_, r| r.plan_revision_id = id(0xf7)),
    ("acceptance", |_, r| {
        r.pricing_acceptance_id = Some(id(0xf14));
    }),
    ("request_digest", |_, r| {
        r.pricing_request_digest = Some(TERMS_DIGEST.to_owned());
    }),
    ("terms_digest", |_, r| {
        r.pricing_terms_digest = Some(REQUEST_DIGEST.to_owned());
    }),
    ("no pin", |_, r| r.order_pin = None),
    ("undecodable pin", |_, r| {
        r.order_pin = Some(json!({"schema_version": 2}));
    }),
    ("authored selection", |_, r| {
        r.selected_items[0]["quantity"] = json!("1");
    }),
];

#[test]
fn every_single_field_corruption_of_a_committed_line_is_integration() {
    for (name, corrupt) in CORRUPTIONS {
        let (mut parent, mut row) = (parent(), row());
        corrupt(&mut parent, &mut row);
        assert!(
            matches!(committed_line(&parent, &row), Err(OrdersError::Integration)),
            "{name}"
        );
    }
}

#[test]
fn a_missing_digest_column_is_integration() {
    for corrupt in [
        |r: &mut order_line::Model| r.pricing_request_digest = None,
        |r: &mut order_line::Model| r.pricing_terms_digest = None,
        |r: &mut order_line::Model| r.pricing_acceptance_id = None,
    ] {
        let mut row = row();
        corrupt(&mut row);
        assert!(matches!(
            committed_line(&parent(), &row),
            Err(OrdersError::Integration)
        ));
    }
}

#[test]
fn a_committed_identity_without_its_row_is_integration() {
    let identities = [identity(id(0xc)), identity(id(0xd))];
    assert!(matches!(
        compose_committed_lines(&parent(), &identities, &[row()]),
        Err(OrdersError::Integration)
    ));
    assert_eq!(
        compose_committed_lines(&parent(), &identities[..1], &[row()])
            .expect("the member composes")
            .len(),
        1
    );
}

#[test]
fn unknown_or_misplaced_total_tokens_are_integration() {
    let lines = [line_of(&row())];
    let cases: [(&str, Vec<resolved_total::Model>); 4] = [
        (
            "unknown line charge kind",
            vec![
                total("line", id(0xc), "bogus"),
                total("line", id(0xc), "recurring"),
                total("order", Uuid::nil(), "recurring"),
            ],
        ),
        (
            "unknown scope",
            vec![
                total("line", id(0xc), "recurring"),
                total("lines", id(0xc), "recurring"),
                total("order", Uuid::nil(), "recurring"),
            ],
        ),
        (
            "an order total with a line id",
            vec![
                total("line", id(0xc), "recurring"),
                total("order", id(0xc), "recurring"),
            ],
        ),
        (
            "no line total",
            vec![total("order", Uuid::nil(), "recurring")],
        ),
    ];
    for (name, totals) in cases {
        assert!(
            matches!(
                validate_totals(&lines, &totals),
                Err(OrdersError::Integration)
            ),
            "{name}"
        );
    }
}

#[test]
fn a_line_kind_without_its_order_roll_up_is_integration() {
    let lines = [line_of(&row())];
    let mut totals = totals();
    totals.push(total("line", id(0xc), "usage"));
    assert!(matches!(
        validate_totals(&lines, &totals),
        Err(OrdersError::Integration)
    ));
    totals.push(total("order", Uuid::nil(), "usage"));
    validate_totals(&lines, &totals).expect("each line kind has its roll-up");
}

/// The check each corruption trips first, in [`CORRUPTIONS`] order (n16, n101).
const CHECKS: [&str; 14] = [
    "seller",
    "payer",
    "resource",
    "order",
    "line",
    "version",
    "plan",
    "revision",
    "acceptance",
    "request_digest",
    "terms_digest",
    "pin",
    "pin",
    "authored selection",
];

#[tracing_test::traced_test]
#[test]
fn a_committed_refusal_logs_the_row_identity_and_the_first_failing_check() {
    for (_, corrupt) in CORRUPTIONS {
        let (mut parent, mut row) = (parent(), row());
        corrupt(&mut parent, &mut row);
        assert!(committed_line(&parent, &row).is_err());
    }
    logs_assert(|lines: &[&str]| {
        let read: Vec<_> = lines
            .iter()
            .filter(|line| line.contains("orders.read.integration"))
            .collect();
        if read.len() != CHECKS.len() {
            return Err(format!("{} integration lines", read.len()));
        }
        for (line, check) in read.iter().zip(CHECKS) {
            if !line.contains(&format!("check=\"{check}\"")) || !line.contains("line_id=") {
                return Err(format!("{check}: {line}"));
            }
            for stored in [REQUEST_DIGEST, TERMS_DIGEST, QUANTITY] {
                if line.contains(stored) {
                    return Err(format!("{check} discloses stored content"));
                }
            }
        }
        Ok(())
    });
    // The undecodable pin carries its codec category, never the stored bytes.
    assert!(logs_contain("invalid commercial encoding (data)"));
}

#[tracing_test::traced_test]
#[test]
fn a_committed_total_with_an_unknown_charge_kind_is_integration() {
    let lines = [line_of(&row())];
    let consistent = vec![
        total("line", id(0xc), "recurring"),
        total("line", id(0xc), "bogus"),
        total("order", Uuid::nil(), "recurring"),
        total("order", Uuid::nil(), "bogus"),
    ];
    assert!(matches!(
        validate_totals(&lines, &consistent),
        Err(OrdersError::Integration)
    ));
    assert!(logs_contain("check=\"charge kind\""));
}

#[tracing_test::traced_test]
#[test]
fn committed_total_refuses_a_scale_outside_u8() {
    let ok = committed_total(&total("line", id(0xc), "recurring")).expect("scale 2 fits a u8");
    assert_eq!(ok.currency_scale, 2);
    for scale in [-1, 256, i32::MAX] {
        let mut row = total("line", id(0xc), "recurring");
        row.currency_scale = scale;
        assert!(matches!(
            committed_total(&row),
            Err(OrdersError::Integration)
        ));
    }
    assert!(logs_contain("check=\"currency_scale\""));
}
