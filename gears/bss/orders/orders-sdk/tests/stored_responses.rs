#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not \
              `expect`, because not every test in the file unwraps or expects"
)]
//! Settled responses are replayed from the idempotency registry for its whole window, so a body
//! written by a newer build (with members this build does not know) must still decode.
use bss_orders_sdk::authoring::{AuthoredTerm, DraftLine, OrderView};
use bss_orders_sdk::catalog::OrderState;
use bss_orders_sdk::models::{SpawnSignalResult, TransitionResult};
use serde_json::json;

#[test]
fn a_stored_transition_result_with_an_unknown_member_still_decodes() {
    let stored = json!({
        "order_id": "00000000-0000-0000-0000-000000000001",
        "state": "draft",
        "version": 1,
        "draft_revision": 2,
        "line_id": "00000000-0000-0000-0000-000000000002",
        "commercial_assessment": {"added_by": "a newer build"},
    });
    let result: TransitionResult = serde_json::from_value(stored).unwrap();
    assert_eq!(result.state, OrderState::Draft);
    assert_eq!(result.draft_revision.map(i64::from), Some(2));
    assert!(result.line_id.is_some());
}

#[test]
fn a_stored_order_view_with_unknown_members_at_every_level_still_decodes() {
    let stored = json!({
        "order": {
            "order_id": "00000000-0000-0000-0000-000000000001",
            "order_number": "ORD-1",
            "state": "draft",
            "category": bss_orders_sdk::authoring::Category::NEW_SALE,
            "resource_tenant_id": "00000000-0000-0000-0000-00000000000a",
            "seller_tenant_id": "00000000-0000-0000-0000-000000000014",
            "payer_tenant_id": "00000000-0000-0000-0000-00000000001e",
            "contract_id": null,
            "sales_path": "self_service",
            "created_at": "2026-10-18T10:00:00Z",
            "state_entered_at": "2026-10-18T10:00:00Z",
            "acceptance_owner": "a newer build",
        },
        "version": {
            "version": 1,
            "supersedes_version": null,
            "created_at": "2026-10-18T10:00:00Z",
            "label": "a newer build",
        },
        "lines": [{
            "line_id": "00000000-0000-0000-0000-000000000002",
            "plan_id": "00000000-0000-0000-0000-000000000003",
            "plan_revision_id": "00000000-0000-0000-0000-000000000004",
            "selected_items": [],
            "currency": "EUR",
            "contract_effective_date": "2026-12-01",
            "service_activation_date": null,
            "acceptance_due_date": null,
            "term_duration": null,
            "billing_cycle": "month",
            "resolved_dates": {"added_by": "a newer build"},
        }],
        "draft_revision": 1,
        "resolved_total": {"added_by": "S6-01"},
    });
    let view: OrderView = serde_json::from_value(stored).unwrap();
    assert_eq!(view.order.order_number, "ORD-1");
    assert_eq!(view.lines.len(), 1);
    assert_eq!(view.draft_revision.map(i64::from), Some(1));
}

/// The members nested in a stored line (its selected items and its authored term) are part of
/// the same replayed body, so they tolerate members from a newer build too.
#[test]
fn a_stored_line_with_unknown_members_in_its_items_and_term_still_decodes() {
    let line = |term: serde_json::Value| {
        json!({
            "line_id": "00000000-0000-0000-0000-000000000002",
            "plan_id": "00000000-0000-0000-0000-000000000003",
            "plan_revision_id": "00000000-0000-0000-0000-000000000004",
            "selected_items": [{
                "item_id": "00000000-0000-0000-0000-000000000006",
                "quantity": "2",
                "selected_dim_value": "eu-west",
                "price_hint": "a newer build",
            }],
            "currency": "EUR",
            "contract_effective_date": null,
            "service_activation_date": null,
            "acceptance_due_date": null,
            "term_duration": term,
            "billing_cycle": "month",
        })
    };
    let terms = [
        (
            json!({"kind": "rolling", "notice": "a newer build"}),
            AuthoredTerm::Rolling,
        ),
        (
            json!({"kind": "periods", "count": 12, "unit_hint": "a newer build"}),
            AuthoredTerm::Periods { count: 12 },
        ),
        (
            json!({"kind": "calendar", "years": 1, "months": 0, "days": 0, "microseconds": 0,
                   "anchor": "a newer build"}),
            AuthoredTerm::Calendar {
                years: 1,
                months: 0,
                days: 0,
                microseconds: 0,
            },
        ),
    ];
    for (stored, expected) in terms {
        let decoded: DraftLine = serde_json::from_value(line(stored)).unwrap();
        assert_eq!(decoded.term_duration, Some(expected));
        let item = &decoded.selected_items[0];
        assert_eq!(
            item.item_id.to_string(),
            "00000000-0000-0000-0000-000000000006"
        );
        assert_eq!(item.selected_dim_value.as_deref(), Some("eu-west"));
        assert!(item.quantity.is_some());
    }
}

#[test]
fn a_stored_spawn_signal_result_with_unknown_members_still_decodes() {
    let stored = json!({
        "transition": {
            "order_id": "00000000-0000-0000-0000-000000000001",
            "state": "in_fulfillment",
            "version": 7,
            "fulfillment_note": "a newer build",
        },
        "spawn_signal_at": "2026-10-05T12:00:00.123456Z",
        "grant_id": "00000000-0000-0000-0000-000000000005",
        "generation": 3,
        "receiver_hint": "a newer build",
    });
    let result: SpawnSignalResult = serde_json::from_value(stored).unwrap();
    assert_eq!(result.transition.state, OrderState::InFulfillment);
    assert_eq!(result.generation, 3);
}

/// `currency_scale` of a committed total is `u8` (A-T5/C9): a stored body with `2` still decodes
/// as the same value, and a scale no `u8` holds is refused at decode, not truncated.
#[test]
fn committed_total_scale_is_u8_and_old_bodies_decode() {
    let total = |scale: serde_json::Value| {
        json!({
            "scope": "line",
            "line_id": "00000000-0000-0000-0000-000000000002",
            "currency": "EUR",
            "assessment_id": "00000000-0000-0000-0000-00000000001e",
            "currency_scale": scale,
            "exclusions": [],
            "item_breakdown": [],
            "recurring_by_cycle": [],
            "amount_status": "committed",
            "amount_basis": "contracted",
            "period_evidence": {},
            "gross_exact": "1",
            "gross_display": "1.00",
            "net_exact": "1",
            "net_display": "1.00",
            "discount_status": "none",
            "discount_exact": null,
            "discount_display": null,
            "promotion_ref": null,
            "charge_kind": "recurring",
            "tcv_exact": null,
            "tcv_display": null,
            "tcv_basis": null,
            "tcv_evidence": null,
        })
    };
    let decoded: bss_orders_sdk::reads::CommittedTotal =
        serde_json::from_value(total(json!(2))).unwrap();
    let scale: u8 = decoded.currency_scale;
    assert_eq!(scale, 2);
    assert_eq!(serde_json::to_value(&decoded).unwrap(), total(json!(2)));
    for bad in [json!(256), json!(-1)] {
        assert!(
            serde_json::from_value::<bss_orders_sdk::reads::CommittedTotal>(total(bad)).is_err()
        );
    }
}
