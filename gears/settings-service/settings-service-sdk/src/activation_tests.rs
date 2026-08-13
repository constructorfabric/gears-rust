// Created: 2026-08-13 by Constructor Tech
//! Tests for the consumer activation contract.
//!
//! Contract source: `DESIGN-activation.md` §4.2 *Consumer Activation SDK* and
//! its back-response contract.
//!
//! The delivery loop itself belongs to the service. What is testable in the SDK
//! is the shape of the contract — and shape is exactly what a later change
//! would break for every consumer that implemented it.

use std::sync::Arc;

use async_trait::async_trait;

use super::{ActivationOutcome, SettingChangeHandler, SettingChangeNotification};
use crate::key::SettingKey;

fn key(instance: &str) -> SettingKey {
    SettingKey::parse(&format!(
        "gts.cf.settings.types.bool_flag.v1~acme.settings.network.{instance}.v1"
    ))
    .expect("fixture key parses")
}

/// A consumer that applies everything it is told about.
struct ApplyEverything;

#[async_trait]
impl SettingChangeHandler for ApplyEverything {
    async fn on_change(&self, notification: SettingChangeNotification) -> Vec<ActivationOutcome> {
        notification
            .changed_keys
            .into_iter()
            .map(|key| ActivationOutcome::Success {
                key,
                applied_value: serde_json::json!(true),
            })
            .collect()
    }
}

#[tokio::test]
async fn a_handler_accounts_for_every_notified_key() {
    // The apply's await-record stays open until each notified key is accounted
    // for, so one outcome per changed key is the handler's whole obligation.
    let notification = SettingChangeNotification {
        apply_id: "apply-1".to_owned(),
        tenant: Some("acme".to_owned()),
        changed_keys: vec![key("enable_proxy"), key("enable_ipv6")],
    };
    let expected = notification.changed_keys.clone();

    let outcomes = ApplyEverything.on_change(notification).await;

    assert_eq!(outcomes.len(), expected.len());
    let accounted: Vec<_> = outcomes.iter().map(ActivationOutcome::key).collect();
    assert_eq!(accounted, expected.iter().collect::<Vec<_>>());
}

#[test]
fn a_handler_is_usable_behind_an_arc() {
    // `subscribe` takes `Arc<dyn SettingChangeHandler>`; a handler that could
    // not be erased behind the trait object would make the method uncallable.
    let handler: Arc<dyn SettingChangeHandler> = Arc::new(ApplyEverything);
    assert_eq!(Arc::strong_count(&handler), 1);
}

#[test]
fn a_notification_carries_no_value_and_no_secret() {
    // The signal stream is identifier-only by construction: consumers re-read
    // under their own identity. A value field here would put tenant data — and
    // eventually a secret — into every subscriber's broker feed.
    let notification = SettingChangeNotification {
        apply_id: "apply-1".to_owned(),
        tenant: Some("acme".to_owned()),
        changed_keys: vec![key("enable_proxy")],
    };
    let json = serde_json::to_value(&notification).expect("serializes");
    let mut fields: Vec<_> = json
        .as_object()
        .expect("is an object")
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        ["applyId", "changedKeys", "tenant"],
        "the notification gained a field; if it carries a value the \
         identifier-only guarantee is gone"
    );
}

#[test]
fn an_absent_tenant_means_platform_wide() {
    let notification = SettingChangeNotification {
        apply_id: "apply-1".to_owned(),
        tenant: None,
        changed_keys: vec![key("enable_proxy")],
    };
    let json = serde_json::to_value(&notification).expect("serializes");
    assert!(json["tenant"].is_null());
}

#[test]
fn an_outcome_names_its_setting_either_way() {
    let applied = ActivationOutcome::Success {
        key: key("enable_proxy"),
        applied_value: serde_json::json!(true),
    };
    let failed = ActivationOutcome::Failed {
        key: key("enable_proxy"),
        detail: "could not rebuild the pool".to_owned(),
    };
    assert_eq!(applied.key(), failed.key());
}

#[test]
fn outcomes_carry_the_back_response_status_vocabulary() {
    // The two variants are the `apply_success` / `apply_failed` back-responses.
    // A consumer of the wire form dispatches on `status`.
    let success = serde_json::to_value(ActivationOutcome::Success {
        key: key("enable_proxy"),
        applied_value: serde_json::json!(true),
    })
    .expect("serializes");
    let failed = serde_json::to_value(ActivationOutcome::Failed {
        key: key("enable_proxy"),
        detail: "nope".to_owned(),
    })
    .expect("serializes");

    assert_eq!(success["status"], "success");
    assert_eq!(failed["status"], "failed");
    assert_eq!(
        success["appliedValue"], true,
        "the echoed value is what the service verifies against its snapshot"
    );
}

#[test]
fn a_success_must_echo_what_was_applied() {
    // A success carrying a value that does not match the apply-time snapshot is
    // treated as a failure by the service, so the field is not optional and a
    // consumer cannot report success without saying what it applied.
    let success = serde_json::to_value(ActivationOutcome::Success {
        key: key("enable_proxy"),
        applied_value: serde_json::json!("sha256:abc"),
    })
    .expect("serializes");
    assert!(
        success.get("appliedValue").is_some(),
        "a success without an applied value cannot be verified"
    );
}
