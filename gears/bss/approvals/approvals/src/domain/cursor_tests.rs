#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bss_approvals_sdk::{Order, SortKey, SourceNarrowing};
use toolkit_canonical_errors::Problem;
use uuid::Uuid;

use super::cursor::{self, DecodedCursor};
use super::query::{self, ListParams};
use crate::test_support;

fn reasons(err: &toolkit_canonical_errors::CanonicalError) -> Vec<String> {
    let problem = Problem::from_error(err).expect("problem");
    problem
        .context
        .get("field_violations")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|violation| {
            violation
                .get("reason")
                .and_then(|reason| reason.as_str())
                .map(str::to_owned)
        })
        .collect()
}

fn sample_key() -> SortKey {
    SortKey {
        submitted_at: test_support::at(5),
        id: Uuid::from_u128(7),
    }
}

fn sample() -> DecodedCursor {
    let mut keys = BTreeMap::new();
    keys.insert("pricing".to_owned(), Some(sample_key()));
    keys.insert("products".to_owned(), None);
    DecodedCursor {
        order: Order::Asc,
        narrowing_hash: cursor::narrowing_hash(&SourceNarrowing::default()),
        keys,
        unavailable: Vec::new(),
    }
}

#[test]
fn the_cursor_round_trips() {
    let cursor = sample();
    let token = cursor::encode(
        cursor.order,
        &cursor.narrowing_hash,
        &cursor.keys,
        &cursor.unavailable,
    )
    .unwrap();
    let decoded = cursor::decode(&token).unwrap();
    assert_eq!(decoded.order, cursor.order);
    assert_eq!(decoded.narrowing_hash, cursor.narrowing_hash);
    assert_eq!(decoded.keys, cursor.keys);
    assert_eq!(decoded.unavailable, cursor.unavailable);
}

#[test]
fn a_different_version_is_rejected() {
    let token = cursor::encode(Order::Desc, "hash", &BTreeMap::new(), &[]).unwrap();
    let mut raw: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&token).unwrap()).unwrap();
    raw["v"] = serde_json::json!(1);
    let tampered = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&raw).unwrap());
    let err = cursor::decode(&tampered).unwrap_err();
    assert_eq!(err.status_code(), 400);
    assert_eq!(reasons(&err), vec!["INVALID_CURSOR".to_owned()]);
}

#[test]
fn tampering_is_rejected() {
    let encoded = URL_SAFE_NO_PAD.encode(b"[]");
    for token in ["!!!", encoded.as_str()] {
        let err = cursor::decode(token).unwrap_err();
        assert_eq!(err.status_code(), 400, "{token}");
        assert_eq!(reasons(&err), vec!["INVALID_CURSOR".to_owned()]);
    }
}

#[test]
fn orderby_with_a_cursor_is_order_with_cursor_before_the_token_is_read() {
    let err = query::prepare_list(&ListParams {
        cursor: Some("not-a-cursor".to_owned()),
        orderby: Some("submitted_at asc".to_owned()),
        ..ListParams::default()
    })
    .unwrap_err();
    assert_eq!(err.status_code(), 400);
    assert!(
        reasons(&err)
            .iter()
            .all(|reason| reason == "ORDER_WITH_CURSOR")
    );
    assert_eq!(reasons(&err).len(), 2);
}

#[test]
fn a_changed_narrowing_is_filter_mismatch() {
    let hash = cursor::narrowing_hash(&SourceNarrowing::default());
    let token = cursor::encode(Order::Asc, &hash, &BTreeMap::new(), &[]).unwrap();
    let err = query::prepare_list(&ListParams {
        state: Some("pending".to_owned()),
        cursor: Some(token),
        ..ListParams::default()
    })
    .unwrap_err();
    assert_eq!(reasons(&err), vec!["FILTER_MISMATCH".to_owned()]);
}

#[test]
fn the_omitted_order_is_newest_first_and_a_bare_submitted_at_is_ascending() {
    let newest = query::prepare_list(&ListParams::default()).unwrap();
    assert_eq!(newest.order, Order::Desc);
    assert!(newest.impact);
    let oldest = query::prepare_list(&ListParams {
        limit: Some(500),
        orderby: Some("submitted_at".to_owned()),
        impact: Some(false),
        ..ListParams::default()
    })
    .unwrap();
    assert_eq!(oldest.order, Order::Asc);
    assert!(!oldest.impact);
    assert_eq!(oldest.limit, 200);
    let err = query::prepare_list(&ListParams {
        orderby: Some("id desc".to_owned()),
        ..ListParams::default()
    })
    .unwrap_err();
    assert_eq!(reasons(&err), vec!["INVALID_ORDERBY_FIELD".to_owned()]);
}

#[test]
fn a_cursor_keeps_its_order_and_starts_a_new_source_at_null() {
    let mut keys = BTreeMap::new();
    keys.insert("pricing".to_owned(), Some(sample_key()));
    keys.insert("gone".to_owned(), Some(sample_key()));
    let hash = cursor::narrowing_hash(&SourceNarrowing::default());
    let token = cursor::encode(Order::Asc, &hash, &keys, &[]).unwrap();
    let prepared = query::prepare_list(&ListParams {
        cursor: Some(token),
        ..ListParams::default()
    })
    .unwrap();
    assert_eq!(prepared.order, Order::Asc);
    assert!(!prepared.keys.contains_key("products"));
    assert!(prepared.keys["pricing"].is_some());
    assert!(prepared.keys["gone"].is_some());
}

#[test]
fn a_zero_limit_is_invalid_limit() {
    let err = query::prepare_list(&ListParams {
        limit: Some(0),
        ..ListParams::default()
    })
    .unwrap_err();
    assert_eq!(err.status_code(), 400);
    assert_eq!(reasons(&err), vec!["INVALID_LIMIT".to_owned()]);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(64))]

    #[test]
    fn a_cursor_round_trips(
        desc in proptest::bool::ANY,
        secs in -1_000_000_i64..1_000_000,
        products_key in proptest::bool::ANY,
        down in proptest::collection::vec("[a-z]{1,6}", 0..3usize),
    ) {
        let order = if desc { Order::Desc } else { Order::Asc };
        let at = time::OffsetDateTime::from_unix_timestamp(secs).unwrap();
        let mut keys = BTreeMap::new();
        keys.insert(
            "pricing".to_owned(),
            Some(SortKey {
                submitted_at: at,
                id: Uuid::from_u128(u128::from(secs.unsigned_abs())),
            }),
        );
        keys.insert(
            "products".to_owned(),
            products_key.then_some(SortKey {
                submitted_at: at,
                id: Uuid::nil(),
            }),
        );
        let token = cursor::encode(order, "hash", &keys, &down).unwrap();
        let decoded = cursor::decode(&token).unwrap();
        proptest::prop_assert_eq!(decoded.order, order);
        proptest::prop_assert_eq!(decoded.narrowing_hash, "hash");
        proptest::prop_assert_eq!(decoded.keys, keys);
        proptest::prop_assert_eq!(decoded.unavailable, down);
    }

    #[test]
    fn decode_of_arbitrary_bytes_does_not_panic(raw in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..48)) {
        let token = URL_SAFE_NO_PAD.encode(&raw);
        assert!(matches!(cursor::decode(&token), Ok(_) | Err(_)));
    }
}
