#![allow(clippy::expect_used)]

use quota_enforcement_sdk::{QuotaId, QuotaStatus, TenantId};
use toolkit_odata::{CursorV1, ODataOrderBy, OrderKey, SortDir};
use uuid::Uuid;

use super::{
    QuotaFilterParts, continuation, lower_quota_filter, next_cursor, page_size, policy_binding,
    require_order,
};

fn lower(raw: &str) -> Result<QuotaFilterParts, String> {
    let expr = toolkit_odata::parse_filter_string(raw)
        .expect("syntax")
        .into_expr();
    lower_quota_filter(Some(&expr)).map_err(|p| format!("{p:?}"))
}

#[test]
fn every_allowed_field_lowers_onto_its_slot() {
    let id = Uuid::from_u128(7);
    let tenant = Uuid::from_u128(9);
    let parts = lower(&format!(
        "tenant_id eq {tenant} and subject_id eq 'u1' and metric eq 'm' \
         and status eq 'deactivated' and id in ({id})"
    ))
    .expect("lowered");
    assert_eq!(parts.tenant_id, Some(TenantId::new(tenant)));
    assert_eq!(parts.subject_id.as_deref(), Some("u1"));
    assert_eq!(parts.metric.as_deref(), Some("m"));
    assert_eq!(parts.status, Some(QuotaStatus::Deactivated));
    assert_eq!(parts.ids, Some(vec![QuotaId::new(id)]));
    assert_eq!(
        lower_quota_filter(None).expect("none"),
        QuotaFilterParts::default()
    );
}

#[test]
fn what_lies_outside_the_grammar_is_refused_naming_the_filter() {
    for raw in [
        "metric ne 'x'",
        "metric eq 'a' or metric eq 'b'",
        "not (metric eq 'a')",
        "metric eq 'a' and metric eq 'b'",
        "id eq 00000000-0000-0000-0000-000000000001 and id in (00000000-0000-0000-0000-000000000002)",
        "metric in ('a')",
        "startswith(metric, 'a')",
        "cap eq 1",
        "tenant_id eq 'x'",
    ] {
        let refusal = lower(raw).expect_err(raw);
        assert!(refusal.contains("INVALID_FILTER"), "{raw}: {refusal}");
    }
    let refusal = lower("status eq 'paused'").expect_err("status");
    assert!(refusal.contains("STATUS_INVALID"), "{refusal}");
}

#[test]
fn the_binding_covers_the_effective_tenant_and_ignores_the_id_order() {
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    let tenant = TenantId::new(Uuid::from_u128(5));
    let forward = lower(&format!("id in ({a}, {b})")).expect("lowered");
    let backward = lower(&format!("id in ({b}, {a})")).expect("lowered");
    assert_eq!(forward.binding(tenant), backward.binding(tenant));
    assert_ne!(
        forward.binding(tenant),
        forward.binding(TenantId::new(Uuid::from_u128(6)))
    );
    assert_ne!(
        forward.binding(tenant),
        QuotaFilterParts::default().binding(tenant)
    );
    assert_ne!(policy_binding("a"), policy_binding("b"));
}

#[test]
fn a_cursor_round_trips_forward_and_refuses_any_other_shape() {
    let token = next_cursor(Some("storage".to_owned()), "+id", "h")
        .expect("encoded")
        .expect("present");
    let cursor = CursorV1::decode(&token).expect("decodes");
    assert_eq!(
        continuation(Some(&cursor), "+id", "h").expect("continues"),
        Some("storage".to_owned())
    );
    assert_eq!(next_cursor(None, "+id", "h").expect("none"), None);

    let refused = |cursor: &CursorV1, sort: &str, binding: &str| {
        format!(
            "{:?}",
            continuation(Some(cursor), sort, binding).expect_err("refused")
        )
    };
    assert!(refused(&cursor, "+id", "other").contains("FILTER_MISMATCH"));
    assert!(refused(&cursor, "+version", "h").contains("ORDER_MISMATCH"));
    let mut backward = cursor.clone();
    backward.d = "bwd".to_owned();
    assert!(refused(&backward, "+id", "h").contains("INVALID_CURSOR"));
    let mut unbound = cursor;
    unbound.f = None;
    assert!(refused(&unbound, "+id", "h").contains("FILTER_MISMATCH"));
}

#[test]
fn only_the_keyset_order_and_a_bounded_page_are_accepted() {
    let order = |field: &str, dir| {
        ODataOrderBy(vec![OrderKey {
            field: field.to_owned(),
            dir,
        }])
    };
    assert!(require_order(&ODataOrderBy::empty(), "id").is_ok());
    assert!(require_order(&order("id", SortDir::Asc), "id").is_ok());
    assert!(require_order(&order("id", SortDir::Desc), "id").is_err());
    assert!(require_order(&order("metric", SortDir::Asc), "id").is_err());

    assert_eq!(page_size(None, 100, 50).expect("default"), 50);
    assert_eq!(page_size(Some(7), 100, 50).expect("explicit"), 7);
    assert!(page_size(Some(51), 100, 50).is_err());
}
