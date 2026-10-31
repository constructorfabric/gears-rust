//! Tests for the consumer activation contract.
//!
//! Contract source: `DESIGN-activation.md` §4.2 *Consumer Activation SDK* and
//! §4.5 *In-Process SDK — Consumer Activation*.
//!
//! The delivery, the consumer group and the registration belong to the
//! service's binding of the client. What is testable in the SDK is the shape of
//! the contract — and shape is exactly what a later change would break for every
//! consumer that implemented it: the wire body, the encoding of an outcome, the
//! handle's states and its drop and retire rules.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::json;
use tokio::sync::watch;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{
    ActivationOutcome, BundledKey, ChangeBundle, ChangeNotification, KeyState, NotifiedKey,
    PendingReason, RefusedReason, ReportStatus, RetireResult, SettingChangeHandler,
    SubscriptionControl, SubscriptionHandle,
};
use crate::canonical::secret_hash;
use crate::key::SettingKey;
use crate::models::{EffectiveSource, EffectiveValueResponse};

fn key(instance: &str) -> SettingKey {
    SettingKey::parse(&format!(
        "gts.cf.core.settings.setting_type.v1~acme.settings.network.{instance}.v1~"
    ))
    .expect("fixture key parses")
}

const TENANT: &str = "11111111-1111-1111-1111-111111111111";

fn value(key: &SettingKey, value: serde_json::Value) -> EffectiveValueResponse {
    EffectiveValueResponse {
        key: key.clone(),
        scope: format!("/tenants/{TENANT}"),
        value,
        source: EffectiveSource::OwnOverride,
        source_scope: None,
        traits: json!({}),
        inheritance_trail: Vec::new(),
    }
}

/// A consumer that applies everything it is told about, and only reports the
/// keys that carry a change to report on.
struct ApplyEverything;

#[async_trait]
impl SettingChangeHandler for ApplyEverything {
    async fn on_change(&self, bundle: ChangeBundle) -> Vec<(SettingKey, ActivationOutcome)> {
        bundle
            .changes
            .into_iter()
            .map(|change| {
                (
                    change.key,
                    ActivationOutcome::Success {
                        applied_value: change.value.value,
                    },
                )
            })
            .collect()
    }
}

#[tokio::test]
async fn a_handler_accounts_for_every_bundled_key() {
    let (a, b) = (key("enable_proxy"), key("enable_ipv6"));
    let bundle = ChangeBundle {
        tenant: TENANT.to_owned(),
        changes: vec![
            BundledKey {
                key: a.clone(),
                change_ref: Some(Uuid::new_v4()),
                value: value(&a, json!(true)),
            },
            BundledKey {
                key: b.clone(),
                change_ref: Some(Uuid::new_v4()),
                value: value(&b, json!(false)),
            },
        ],
    };

    let outcomes = ApplyEverything.on_change(bundle).await;

    let accounted: Vec<_> = outcomes.iter().map(|(key, _)| key.clone()).collect();
    assert_eq!(accounted, [a, b]);
}

#[test]
fn a_handler_is_usable_behind_an_arc() {
    // `subscribe` takes `Arc<dyn SettingChangeHandler>`; a handler that could
    // not be erased behind the trait object would make the method uncallable.
    let handler: Arc<dyn SettingChangeHandler> = Arc::new(ApplyEverything);
    assert_eq!(Arc::strong_count(&handler), 1);
}

// ---------------------------------------------------------------------------
// The wire body
// ---------------------------------------------------------------------------

#[test]
fn a_notification_carries_identifiers_only() {
    // The signal stream is identifier-only by construction: consumers re-read
    // under their own identity. A value field here would put tenant data — and
    // eventually a secret — into every subscriber's broker feed.
    let notification = ChangeNotification {
        tenant: TENANT.to_owned(),
        subscriber: "gear-a".to_owned(),
        changes: vec![NotifiedKey {
            key: key("enable_proxy").as_str().to_owned(),
            change_ref: Uuid::nil(),
        }],
        subject_type: None,
        subject_id: None,
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
        ["changes", "subscriber", "tenant"],
        "the notification gained a field; if it carries a value the \
         identifier-only guarantee is gone"
    );
    let mut entry: Vec<_> = json["changes"][0]
        .as_object()
        .expect("an entry")
        .keys()
        .map(String::as_str)
        .collect();
    entry.sort_unstable();
    assert_eq!(entry, ["change_ref", "key"]);
}

#[test]
fn the_tenant_is_never_absent() {
    // Platform scope is the root tenant's id, not a missing tenant: a
    // notification always says which tenant changed, so a consumer never has to
    // infer "platform-wide" from an absence.
    let without = json!({ "subscriber": "g", "changes": [] });
    assert!(serde_json::from_value::<ChangeNotification>(without).is_err());
}

#[test]
fn a_notification_ignores_attributes_it_does_not_know_and_reads_the_optional_subject() {
    let wire = json!({
        "tenant": TENANT,
        "subscriber": "gear-a",
        "changes": [{ "key": "k", "change_ref": Uuid::nil(), "later": 1 }],
        "subject_type": "gts.x~",
        "subject_id": "s-1",
        "from_a_newer_service": true
    });
    let parsed: ChangeNotification = serde_json::from_value(wire).expect("tolerant reader");
    assert_eq!(parsed.subject_type.as_deref(), Some("gts.x~"));
    assert_eq!(parsed.subject_id.as_deref(), Some("s-1"));
    assert_eq!(parsed.changes.len(), 1);
}

#[test]
fn a_notification_without_a_change_ref_or_with_a_malformed_one_does_not_parse() {
    for changes in [
        json!([{ "key": "k" }]),
        json!([{ "key": "k", "change_ref": "not-a-uuid" }]),
        json!("a list is expected"),
    ] {
        let wire = json!({ "tenant": TENANT, "subscriber": "g", "changes": changes });
        assert!(serde_json::from_value::<ChangeNotification>(wire).is_err());
    }
}

// ---------------------------------------------------------------------------
// Encoding an outcome
// ---------------------------------------------------------------------------

#[test]
fn a_success_is_reported_as_the_canonical_encoding_of_what_was_applied() {
    let encoded = ActivationOutcome::Success {
        applied_value: json!({ "b": [1.0, 2], "a": "\u{e9}" }),
    }
    .encode(&key("proxy"), TENANT)
    .expect("encodes");
    assert_eq!(encoded.status, ReportStatus::Success);
    assert_eq!(
        encoded.applied_value.as_deref(),
        Some("{\"a\":\"\u{e9}\",\"b\":[1,2]}")
    );
    assert_eq!(encoded.detail, None);
}

#[test]
fn a_secret_success_is_reported_as_the_salted_hash_never_the_plaintext() {
    let k = key("password");
    let encoded = ActivationOutcome::SecretSuccess {
        plaintext: SecretString::from("hunter2"),
    }
    .encode(&k, TENANT)
    .expect("encodes");
    let hash = secret_hash(k.as_str(), TENANT, &SecretString::from("hunter2")).expect("hashes");
    assert_eq!(encoded.applied_value.as_deref(), Some(hash.as_str()));
    assert!(!format!("{encoded:?}").contains("hunter2"));
    assert!(!format!("{encoded:?}").contains(&hash));
}

#[test]
fn the_secret_salt_is_the_tenant_as_a_bare_lowercase_uuid() {
    let k = key("password");
    let plaintext = || ActivationOutcome::SecretSuccess {
        plaintext: SecretString::from("hunter2"),
    };
    let lower = plaintext().encode(&k, TENANT).expect("encodes");
    let upper = plaintext()
        .encode(&k, &TENANT.to_uppercase())
        .expect("a tenant in another spelling is normalised");
    let braced = plaintext()
        .encode(&k, &format!("{{{TENANT}}}"))
        .expect("encodes");
    assert_eq!(lower, upper);
    assert_eq!(lower, braced);
}

#[test]
fn a_failure_reports_its_detail_and_no_value() {
    let encoded = ActivationOutcome::Failed {
        detail: "could not rebuild the pool".to_owned(),
    }
    .encode(&key("proxy"), TENANT)
    .expect("encodes");
    assert_eq!(encoded.status, ReportStatus::Failed);
    assert_eq!(encoded.applied_value, None);
    assert_eq!(
        encoded.detail.as_deref(),
        Some("could not rebuild the pool")
    );
}

#[test]
fn an_outcome_that_cannot_be_encoded_is_refused_not_rounded() {
    let beyond = serde_json::from_str("9007199254740993").expect("JSON");
    assert!(
        ActivationOutcome::Success {
            applied_value: beyond
        }
        .encode(&key("proxy"), TENANT)
        .is_err()
    );
    assert!(
        ActivationOutcome::SecretSuccess {
            plaintext: SecretString::from("x")
        }
        .encode(&key("proxy"), "not-a-uuid")
        .is_err(),
        "a secret cannot be salted by a tenant that is not one"
    );
    // The error names the field at fault: the tenant, not the applied value.
    let tenant_fault = ActivationOutcome::SecretSuccess {
        plaintext: SecretString::from("x"),
    }
    .encode(&key("proxy"), "not-a-uuid")
    .expect_err("a malformed tenant");
    assert_eq!(tenant_fault.field(), "tenant");
    let value_fault = ActivationOutcome::Success {
        applied_value: serde_json::from_str("9007199254740993").expect("a number"),
    }
    .encode(&key("proxy"), TENANT)
    .expect_err("beyond 2^53");
    assert_eq!(value_fault.field(), "applied_value");
}

#[test]
fn an_outcome_never_prints_a_value_a_plaintext_or_a_detail() {
    let outcomes = [
        ActivationOutcome::Success {
            applied_value: json!("applied-value-marker"),
        },
        ActivationOutcome::SecretSuccess {
            plaintext: SecretString::from("plaintext-marker"),
        },
        ActivationOutcome::Failed {
            detail: "detail-marker".to_owned(),
        },
    ];
    for outcome in outcomes {
        let printed = format!("{outcome:?}");
        for marker in ["applied-value-marker", "plaintext-marker", "detail-marker"] {
            assert!(!printed.contains(marker), "{printed}");
        }
    }
}

// ---------------------------------------------------------------------------
// The handle
// ---------------------------------------------------------------------------

/// A control that records what the handle asks of it.
struct Control {
    states: std::sync::Mutex<BTreeMap<SettingKey, KeyState>>,
    version: watch::Sender<u64>,
    closed: AtomicUsize,
}

impl Control {
    fn new(states: BTreeMap<SettingKey, KeyState>) -> Arc<Self> {
        Arc::new(Self {
            states: std::sync::Mutex::new(states),
            version: watch::channel(0).0,
            closed: AtomicUsize::new(0),
        })
    }

    fn set(&self, key: &SettingKey, state: KeyState) {
        self.states.lock().unwrap().insert(key.clone(), state);
        self.version.send_modify(|v| *v += 1);
    }
}

#[async_trait]
impl SubscriptionControl for Control {
    fn states(&self) -> BTreeMap<SettingKey, KeyState> {
        self.states.lock().unwrap().clone()
    }
    fn watch(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }
    async fn retire(
        &self,
        _: &SecurityContext,
    ) -> Result<BTreeMap<SettingKey, RetireResult>, CanonicalError> {
        Ok(self
            .states
            .lock()
            .unwrap()
            .keys()
            .map(|k| (k.clone(), RetireResult::Retired))
            .collect())
    }
    fn close(&self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn the_per_key_states_carry_the_reasons_of_the_results_table() {
    // The §4.5 table: `registered` (with or without a value), `pending` for the
    // four things a key waits for, `refused` for the two terminal ones.
    let (a, b, c) = (key("a"), key("b"), key("c"));
    let control = Control::new(BTreeMap::from([
        (
            a.clone(),
            KeyState::Registered {
                value: Some(value(&a, json!(1))),
            },
        ),
        (b.clone(), KeyState::Registered { value: None }),
        (
            c.clone(),
            KeyState::Pending {
                reason: PendingReason::NotFound,
            },
        ),
    ]));
    let handle = SubscriptionHandle::new(control);
    let states = handle.states();
    assert!(matches!(
        states[&a],
        KeyState::Registered { value: Some(_) }
    ));
    assert!(matches!(states[&b], KeyState::Registered { value: None }));
    assert!(matches!(
        states[&c],
        KeyState::Pending {
            reason: PendingReason::NotFound
        }
    ));
    for reason in [
        PendingReason::Registering,
        PendingReason::NotFound,
        PendingReason::Unavailable,
        PendingReason::FreshReadFailed,
        PendingReason::BrokerGrantMissing,
    ] {
        assert!(!format!("{:?}", KeyState::Pending { reason }).is_empty());
    }
    for reason in [RefusedReason::NotReadable, RefusedReason::InterestLimit] {
        assert!(!format!("{:?}", KeyState::Refused { reason }).is_empty());
    }
}

#[tokio::test]
async fn a_handle_wakes_when_a_key_changes_state() {
    let a = key("a");
    let control = Control::new(BTreeMap::from([(
        a.clone(),
        KeyState::Pending {
            reason: PendingReason::NotFound,
        },
    )]));
    let mut handle = SubscriptionHandle::new(Arc::clone(&control) as Arc<dyn SubscriptionControl>);
    let waiting = tokio::spawn(async move {
        assert!(handle.changed().await);
        handle
    });
    tokio::task::yield_now().await;
    control.set(&a, KeyState::Registered { value: None });
    let handle = waiting.await.expect("the waiter finished");
    assert!(matches!(
        handle.states()[&a],
        KeyState::Registered { value: None }
    ));
}

#[tokio::test]
async fn dropping_a_handle_closes_it_and_retires_nothing() {
    let control = Control::new(BTreeMap::from([(
        key("a"),
        KeyState::Registered { value: None },
    )]));
    let handle = SubscriptionHandle::new(Arc::clone(&control) as Arc<dyn SubscriptionControl>);
    drop(handle);
    assert_eq!(control.closed.load(Ordering::SeqCst), 1);
    assert_eq!(
        control.states.lock().unwrap().len(),
        1,
        "a drop deletes no subscription"
    );
}

#[tokio::test]
async fn retire_is_explicit_and_answers_per_key() {
    let (a, b) = (key("a"), key("b"));
    let control = Control::new(BTreeMap::from([
        (a.clone(), KeyState::Registered { value: None }),
        (b.clone(), KeyState::Registered { value: None }),
    ]));
    let handle = SubscriptionHandle::new(control);
    let answers = handle
        .retire(&SecurityContext::anonymous())
        .await
        .expect("answers");
    assert_eq!(answers[&a], RetireResult::Retired);
    assert_eq!(answers[&b], RetireResult::Retired);
}
