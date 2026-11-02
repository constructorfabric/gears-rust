#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_rating_core::engine::current::input::{AggregationKey, AggregationScope, TenantAxes};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::{instant_text, window_child_key};
use crate::domain::digest::{Sha256Hex, canonical_json_text};

fn key(meter: &str) -> AggregationKey {
    AggregationKey {
        tenant_axes: TenantAxes {
            seller_tenant_id: Uuid::from_u128(1),
            payer_tenant_id: Uuid::from_u128(2),
            resource_tenant_id: Uuid::from_u128(3),
        },
        subscription_id: Uuid::from_u128(4),
        sub_line_key: "plan#1".to_owned(),
        item_id: Uuid::from_u128(5),
        meter: Some(meter.to_owned()),
        dimension_key: None,
        scope: AggregationScope::SubscriptionLine,
    }
}

fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &Rfc3339).unwrap()
}

#[test]
fn the_canonical_instant_text_is_utc_whole_seconds_with_z() {
    assert_eq!(
        instant_text(at("2026-09-15T00:00:00Z")),
        "2026-09-15T00:00:00Z"
    );
    assert_eq!(
        instant_text(at("2026-09-15T02:00:00+02:00")),
        "2026-09-15T00:00:00Z"
    );
}

#[test]
fn a_child_id_is_the_uuid_v5_of_its_wk1_name() {
    let fact = Uuid::from_u128(0xf);
    let start = at("2026-10-01T00:00:00Z");
    let one = window_child_key(fact, start, &key("products.derived/a@1")).unwrap();
    let again = window_child_key(fact, start, &key("products.derived/a@1")).unwrap();
    assert_eq!(one, again);
    let agg = canonical_json_text(&serde_json::to_value(key("products.derived/a@1")).unwrap());
    let name = format!("wk1|{fact}|2026-10-01T00:00:00Z|{agg}");
    assert_eq!(
        one.child_id,
        Uuid::new_v5(&super::NS_RATING_CHILD, name.as_bytes())
    );
    assert_eq!(one.agg_key_digest, Sha256Hex::of_bytes(agg.as_bytes()));
}

#[test]
fn the_window_and_the_key_each_change_the_id() {
    let fact = Uuid::from_u128(0xf);
    let start = at("2026-10-01T00:00:00Z");
    let base = window_child_key(fact, start, &key("products.derived/a@1")).unwrap();
    let later = window_child_key(
        fact,
        at("2026-10-01T01:00:00Z"),
        &key("products.derived/a@1"),
    )
    .unwrap();
    let other = window_child_key(fact, start, &key("products.derived/b@1")).unwrap();
    assert_ne!(base.child_id, later.child_id);
    assert_ne!(base.child_id, other.child_id);
    assert_eq!(base.agg_key_digest, later.agg_key_digest);
}
