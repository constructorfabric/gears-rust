//! Tests for the tenant posting-policy value types ([`super`]).

use super::*;

#[test]
fn missing_mapping_mode_round_trips() {
    assert_eq!(
        MissingMappingMode::parse("SUSPENSE"),
        Ok(MissingMappingMode::Suspense)
    );
    assert_eq!(
        MissingMappingMode::parse("HARD_BLOCK"),
        Ok(MissingMappingMode::HardBlock)
    );
    assert_eq!(MissingMappingMode::Suspense.as_str(), "SUSPENSE");
    assert_eq!(MissingMappingMode::HardBlock.as_str(), "HARD_BLOCK");
    assert!(MissingMappingMode::parse("nope").is_err());
    assert!(
        MissingMappingMode::parse("suspense").is_err(),
        "case-sensitive"
    );
}

#[test]
fn missing_mapping_mode_default_is_suspense() {
    assert_eq!(MissingMappingMode::default(), MissingMappingMode::Suspense);
}

#[test]
fn aging_thresholds_default_is_the_classic_buckets() {
    assert_eq!(AgingThresholds::default().bounds(), &[30, 60, 90]);
}

#[test]
fn aging_thresholds_csv_round_trips_and_tolerates_whitespace() {
    let t = AgingThresholds::parse_csv("30,60,90").expect("valid");
    assert_eq!(t.bounds(), &[30, 60, 90]);
    assert_eq!(t.to_csv(), "30,60,90");
    assert_eq!(
        AgingThresholds::parse_csv(" 15 , 45 ")
            .expect("valid")
            .bounds(),
        &[15, 45]
    );
}

#[test]
fn aging_thresholds_reject_invalid() {
    assert!(AgingThresholds::new(vec![]).is_err(), "empty");
    assert!(
        AgingThresholds::new(vec![0, 30]).is_err(),
        "non-positive first"
    );
    assert!(AgingThresholds::new(vec![-5]).is_err(), "negative");
    assert!(
        AgingThresholds::new(vec![60, 30]).is_err(),
        "non-increasing"
    );
    assert!(AgingThresholds::new(vec![30, 30]).is_err(), "duplicate");
    assert!(
        AgingThresholds::parse_csv("30,x,90").is_err(),
        "non-numeric"
    );
    let max_bounds = i64::try_from(AgingThresholds::MAX_BOUNDS).expect("MAX_BOUNDS fits i64");
    let over_cap: Vec<i64> = (1..=(max_bounds + 1)).collect();
    assert!(
        AgingThresholds::new(over_cap).is_err(),
        "over the bucket cap"
    );
}

#[test]
fn posting_policy_default_is_suspense_and_classic_buckets() {
    let p = PostingPolicy::default();
    assert_eq!(p.missing_mapping_mode, MissingMappingMode::Suspense);
    assert_eq!(p.aging_thresholds.bounds(), &[30, 60, 90]);
}
