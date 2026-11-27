use std::time::Duration;

use super::*;

#[test]
fn defaults_match_the_documented_limits_and_are_valid() {
    let cfg = AuthZResolverConfig::default();
    assert_eq!(cfg.batch.max_items, 100);
    assert_eq!(cfg.batch.max_body_bytes, 1_048_576);
    assert_eq!(cfg.batch.max_concurrency, 8);
    assert_eq!(cfg.batch.max_in_flight, 64);
    assert_eq!(cfg.batch.deadline, Duration::from_secs(20));
    cfg.validate().unwrap();
}

#[test]
fn an_older_configuration_without_batch_stays_valid() {
    let cfg: AuthZResolverConfig =
        serde_json::from_value(serde_json::json!({ "vendor": "acme" })).unwrap();
    assert_eq!(cfg.vendor, "acme");
    assert_eq!(cfg.batch.max_items, 100);
}

#[test]
fn batch_keys_parse_including_humantime_deadline() {
    let cfg: AuthZResolverConfig = serde_json::from_value(serde_json::json!({
        "batch": {
            "max_items": 10,
            "max_body_bytes": 2048,
            "max_concurrency": 2,
            "max_in_flight": 4,
            "deadline": "1500ms"
        }
    }))
    .unwrap();
    assert_eq!(cfg.batch.max_items, 10);
    assert_eq!(cfg.batch.max_body_bytes, 2048);
    assert_eq!(cfg.batch.max_concurrency, 2);
    assert_eq!(cfg.batch.max_in_flight, 4);
    assert_eq!(cfg.batch.deadline, Duration::from_millis(1500));
    cfg.validate().unwrap();
}

#[test]
fn unknown_batch_keys_are_rejected() {
    let err = serde_json::from_value::<AuthZResolverConfig>(serde_json::json!({
        "batch": { "max_itemz": 10 }
    }))
    .unwrap_err();
    assert!(err.to_string().contains("max_itemz"), "{err}");
}

type Case = (fn(&mut BatchConfig), &'static str);

#[test]
fn out_of_range_bounds_are_rejected_naming_the_key() {
    let cases: [Case; 8] = [
        (|b| b.max_items = 0, "max_items"),
        (|b| b.max_items = MAX_BATCH_ITEMS + 1, "max_items"),
        (|b| b.max_body_bytes = 0, "max_body_bytes"),
        (|b| b.max_concurrency = 0, "max_concurrency"),
        (
            |b| {
                b.max_concurrency = MAX_BATCH_CONCURRENCY + 1;
                b.max_in_flight = MAX_BATCH_IN_FLIGHT;
            },
            "max_concurrency",
        ),
        (|b| b.max_in_flight = 7, "max_in_flight"),
        (
            |b| b.max_in_flight = MAX_BATCH_IN_FLIGHT + 1,
            "max_in_flight",
        ),
        (|b| b.deadline = Duration::ZERO, "deadline"),
    ];
    for (mutate, key) in cases {
        let mut batch = BatchConfig::default();
        mutate(&mut batch);
        let err = batch.validate().unwrap_err();
        assert_eq!(err.key, key, "{err}");
        assert!(err.to_string().contains(key), "{err}");
    }
}

#[test]
fn inclusive_edges_are_accepted() {
    let cases: [fn(&mut BatchConfig); 6] = [
        |b| b.max_items = 1,
        |b| b.max_items = MAX_BATCH_ITEMS,
        |b| {
            b.max_concurrency = 1;
            b.max_in_flight = 1;
        },
        |b| {
            b.max_concurrency = MAX_BATCH_CONCURRENCY;
            b.max_in_flight = MAX_BATCH_CONCURRENCY;
        },
        |b| b.max_in_flight = MAX_BATCH_IN_FLIGHT,
        |b| b.max_body_bytes = 1,
    ];
    for mutate in cases {
        let mut batch = BatchConfig::default();
        mutate(&mut batch);
        batch.validate().unwrap();
    }
}
