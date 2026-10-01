#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use admission_control_sdk::AdmissionRequest;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{BuiltinOutcome, BuiltinPolicySet};
use crate::config::{AdmissionControlConfig, BuiltinPolicyConfig};

const WIDGET: &str = "gts.cf.core.test.widget.v1~";
const RESERVED: &str = r#"
package builtin.reserved

deny if {
    input.action == "create"
    startswith(input.properties.name, "cf-")
}
"#;

fn policy(
    id: &str,
    resource_types: &[&str],
    actions: &[&str],
    content: &str,
) -> BuiltinPolicyConfig {
    BuiltinPolicyConfig {
        id: id.to_owned(),
        description: None,
        resource_types: resource_types.iter().map(|s| (*s).to_owned()).collect(),
        actions: actions.iter().map(|s| (*s).to_owned()).collect(),
        content: content.to_owned(),
    }
}

fn set(policies: Vec<BuiltinPolicyConfig>) -> BuiltinPolicySet {
    AdmissionControlConfig {
        builtin_policies: policies,
        ..Default::default()
    }
    .compile_builtins()
    .unwrap()
}

fn evaluate(
    set: &BuiltinPolicySet,
    resource_type: &str,
    action: &str,
    name: &str,
) -> BuiltinOutcome {
    let request = AdmissionRequest::new("gear", action, resource_type, Uuid::nil())
        .with_property("name", json!(name));
    set.evaluate(
        &request,
        (Uuid::from_u128(1), Uuid::from_u128(2)),
        OffsetDateTime::UNIX_EPOCH,
        Duration::from_millis(500),
    )
}

#[test]
fn denies_silent_and_unselected_requests() {
    let set = set(vec![policy("reserved", &[WIDGET], &["create"], RESERVED)]);
    assert_eq!(
        evaluate(&set, WIDGET, "create", "cf-x"),
        BuiltinOutcome::Prohibited {
            policy_id: "reserved".to_owned()
        }
    );
    assert_eq!(
        evaluate(&set, WIDGET, "create", "x"),
        BuiltinOutcome::NoProhibition
    );
    assert_eq!(
        evaluate(&set, WIDGET, "delete", "cf-x"),
        BuiltinOutcome::NoProhibition
    );
    assert_eq!(
        evaluate(&set, "gts.cf.core.test.other.v1~", "create", "cf-x"),
        BuiltinOutcome::NoProhibition
    );
}

#[test]
fn wildcard_selectors_match_and_first_denial_wins() {
    let set = set(vec![
        policy(
            "first",
            &["gts.cf.core.test.*"],
            &[],
            "package a\ndeny := true",
        ),
        policy("second", &[WIDGET], &[], "package b\ndeny := true"),
    ]);
    assert_eq!(
        evaluate(&set, WIDGET, "create", "x"),
        BuiltinOutcome::Prohibited {
            policy_id: "first".to_owned()
        }
    );
}

#[test]
fn evaluation_failure_is_reported_as_failed() {
    let set = set(vec![
        policy("broken", &[WIDGET], &[], "package a\ndeny := 1"),
        policy("never", &[WIDGET], &[], "package b\ndeny := true"),
    ]);
    assert_eq!(
        evaluate(&set, WIDGET, "create", "x"),
        BuiltinOutcome::Failed {
            policy_id: "broken".to_owned()
        }
    );
}

#[test]
fn concrete_resource_types_exclude_wildcards() {
    let set = set(vec![policy(
        "p",
        &[WIDGET, "gts.cf.core.other.*"],
        &[],
        "package a\ndeny := false",
    )]);
    assert_eq!(set.concrete_resource_types().collect::<Vec<_>>(), [WIDGET]);
}
