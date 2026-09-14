//! The per-column fold and the two column readings, without a database.
//!
//! D-152's "a tenant with no entry takes the ratified launch default" is a **per
//! column** promise: a tenant that configured one cap did not thereby ask for the
//! other three to change, and a fold that fell back per row would pass any test
//! written against a tenant with no row at all.
//!
//! **The partial row is not here**, and cannot be: `authoring_policy_on` takes a
//! runner, so the composition of the four columns is only reachable with a
//! database.
//! `tests/sqlite_policy_object.rs::a_configured_band_and_row_cap_reach_the_policy`
//! is what stores two caps, leaves two null and asserts the answer is per column.
//! What this file owns is the two primitives underneath it - [`cap`]'s
//! absent-versus-configured reading and its refusal of a non-positive stored
//! value, and [`read_required_keys`].

#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::json;

use super::{AuthoringPolicy, cap, read_required_keys};
use crate::config::LimitsConfig;
use crate::domain::plan_rules::DescriptorSetComplete;
use crate::infra::storage::RepoError;

#[test]
fn an_absent_cap_is_the_ratified_default_and_a_configured_one_replaces_it() {
    assert_eq!(cap("column", None, 366).expect("absent"), 366);
    assert_eq!(cap("column", Some(90), 366).expect("configured"), 90);
    // Above the default as well as below it: a cap is a tenant policy, not a
    // ceiling the deployment imposes on what a tenant may raise it to.
    assert_eq!(cap("column", Some(500), 366).expect("configured"), 500);
}

/// The column's `CHECK` refuses both, so either reading means the table was
/// written around. Falling back to the default instead would make a cap nobody
/// can satisfy indistinguishable from a cap nobody configured.
#[test]
fn a_zero_or_negative_cap_is_a_corrupt_row_and_not_a_silent_fallback() {
    for stored in [0, -1] {
        let err = cap(
            "pricing_policy_object.max_price_rows_per_plan",
            Some(stored),
            500,
        )
        .expect_err("a non-positive cap must not resolve");
        assert!(
            matches!(&err, RepoError::CorruptRow(detail)
                if detail.contains("max_price_rows_per_plan") && detail.contains("positive cap")),
            "got: {err:?}"
        );
    }
}

#[test]
fn the_required_key_list_is_read_in_order_and_de_duplicated() {
    assert!(
        read_required_keys(&json!([]))
            .expect("empty array")
            .is_empty()
    );
    assert_eq!(
        read_required_keys(&json!(["costCentre", "profitCentre", "costCentre"]))
            .expect("array of names"),
        ["costCentre", "profitCentre"],
        "first-seen order is kept, and the duplicate is not a second requirement"
    );
}

/// A malformed entry must not read as a tenant that configured nothing: that
/// would drop a required descriptor key and publish a plan the tenant's own
/// policy blocks.
#[test]
fn a_column_that_is_not_an_array_of_strings_is_a_corrupt_row() {
    for stored in [json!({}), json!("costCentre"), json!(["costCentre", 4])] {
        let err = read_required_keys(&stored).expect_err("a malformed column must not resolve");
        assert!(matches!(err, RepoError::CorruptRow(_)), "got: {err:?}");
    }
}

/// The deployment section has no descriptor-extension field, and must not grow
/// one: a per-deployment extension would let one deployment's tenants share a
/// Billing contract they do not share.
#[test]
fn the_deployment_defaults_carry_the_four_caps_and_no_descriptor_extension() {
    let limits = LimitsConfig::default();
    let policy = AuthoringPolicy::from_deployment_defaults(&limits);

    assert_eq!(
        policy.max_tier_bands_per_row(),
        limits.max_tier_bands_per_row
    );
    assert_eq!(
        policy.max_price_rows_per_plan(),
        limits.max_price_rows_per_plan
    );
    assert!(policy.additional_required_descriptors().is_empty());
    assert_eq!(
        policy.descriptor_rule(),
        DescriptorSetComplete::default(),
        "no entry means D-48's pinned three and nothing more"
    );
}
