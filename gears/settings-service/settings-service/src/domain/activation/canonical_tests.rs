//! The SDK's conformance corpus, run against what this service stores.
//!
//! The encoder is one implementation, so agreement of the two sides is by
//! construction; these tests pin that the *service's* path — the value a change
//! actually writes into `expected_value`, through `record_change` — is that
//! encoder's output for every case of the shared corpus, and that the SDK's
//! encoding of the consumer's report of the same value equals it.

use std::sync::Arc;

use secrecy::SecretString;
use serde_json::{Value, json};
use settings_service_sdk::ActivationOutcome;
use uuid::Uuid;

use super::{ExpectedValue, canonical_json, secret_hash};
use crate::domain::activation::{
    ActivationPublisher, ActivationRepository, Qualified, ReadGate, RecordedChange,
    SubscriptionManager, tenant_text,
};
use crate::domain::declaration::DeclarationRepository;
use crate::domain::resolution::scope_class;
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::test_support::{FakeReadGate, ResolutionHarness, SECRET, TEXT};

/// The corpus the SDK's own tests run.
const CORPUS: &str = include_str!("../../../../settings-service-sdk/src/canonical_corpus.json");

fn corpus() -> Value {
    serde_json::from_str(CORPUS).expect("the corpus is JSON")
}

/// Store `value` at the root as a change would and record the change for one
/// subscriber; the `expected_value` the state holds afterwards.
async fn stored_expected_value(
    harness: &ResolutionHarness,
    name: &str,
    value_type: &str,
    value: Value,
    plaintext: Option<&SecretString>,
) -> (String, String) {
    use toolkit_security::AccessScope;
    let root = harness.tree.root;
    let id = harness
        .declare_typed(
            name,
            scope_class::CASCADING,
            json!(""),
            value_type,
            if value_type == SECRET {
                "secret"
            } else {
                "public"
            },
        )
        .await;
    let conn = harness.db.conn().expect("connection");
    ActivationRepo
        .register(
            &conn,
            "gear",
            harness.key(name).as_str(),
            &tenant_text(root),
        )
        .await
        .expect("registered");
    let manager = SubscriptionManager::new(
        Arc::clone(&harness.resolver),
        ActivationRepo,
        FakeReadGate::allowing() as Arc<dyn ReadGate>,
    );
    let declaration = DeclarationRepo
        .find_by_key(&conn, &AccessScope::allow_all(), harness.key(name).as_str())
        .await
        .expect("lookup")
        .expect("declared");
    let qualified: Vec<Qualified> = manager
        .resolve_subscribers(&conn, &declaration, root)
        .await
        .expect("qualified");
    if value_type == SECRET {
        harness.set_secret(id, root, "ref-1").await;
    } else {
        harness.set(id, root, value).await;
    }
    ActivationPublisher::new(Arc::clone(&harness.resolver), ActivationRepo)
        .record_change(
            &conn,
            &RecordedChange {
                declaration: &declaration,
                written: root,
                root,
                pre_image: None,
                change_ref: Uuid::new_v4(),
                qualified: &qualified,
                plaintext,
            },
        )
        .await
        .expect("recorded");
    let state = ActivationRepo
        .state(
            &conn,
            "gear",
            harness.key(name).as_str(),
            &tenant_text(root),
        )
        .await
        .expect("state")
        .expect("a state was written");
    (state.expected_value, tenant_text(root))
}

#[tokio::test]
async fn every_corpus_value_is_stored_as_its_canonical_text_and_reported_as_the_same_bytes() {
    let harness = ResolutionHarness::new().await;
    let corpus = corpus();
    for (index, case) in corpus["values"]
        .as_array()
        .expect("values")
        .iter()
        .enumerate()
    {
        let value: Value =
            serde_json::from_str(case["input"].as_str().expect("input")).expect("JSON");
        let (stored, tenant) = stored_expected_value(
            &harness,
            &format!("corpus_{index}"),
            TEXT,
            value.clone(),
            None,
        )
        .await;
        assert_eq!(
            stored,
            case["canonical"].as_str().expect("canonical"),
            "{}",
            case["name"]
        );
        // The consumer reports the value it applied; the SDK encodes it.
        let reported = ActivationOutcome::Success {
            applied_value: value,
        }
        .encode(&harness.key(&format!("corpus_{index}")), &tenant)
        .expect("encodes")
        .applied_value;
        assert_eq!(
            reported.as_deref(),
            Some(stored.as_str()),
            "{}",
            case["name"]
        );
    }
}

#[tokio::test]
async fn every_corpus_secret_is_stored_as_the_hash_the_sdk_reports() {
    let harness = ResolutionHarness::new().await;
    let corpus = corpus();
    for (index, case) in corpus["secrets"]
        .as_array()
        .expect("secrets")
        .iter()
        .enumerate()
    {
        let plaintext = SecretString::from(case["plaintext"].as_str().expect("plaintext"));
        let name = format!("secret_{index}");
        let (stored, tenant) =
            stored_expected_value(&harness, &name, SECRET, Value::Null, Some(&plaintext)).await;
        let key = harness.key(&name);
        assert_eq!(
            stored,
            secret_hash(key.as_str(), &tenant, &plaintext).expect("hashes"),
            "{}",
            case["name"]
        );
        let reported = ActivationOutcome::SecretSuccess { plaintext }
            .encode(&key, &tenant)
            .expect("encodes")
            .applied_value;
        assert_eq!(
            reported.as_deref(),
            Some(stored.as_str()),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn a_file_reference_value_is_an_ordinary_object_with_no_hash() {
    let reference = json!({ "file_id": "f-1", "version": 3, "name": "ca.pem" });
    let expected = ExpectedValue::of_value(&reference).expect("encodes");
    assert_eq!(
        expected.as_str(),
        r#"{"file_id":"f-1","name":"ca.pem","version":3}"#
    );
    assert_eq!(
        canonical_json(&reference).expect("canonical"),
        expected.as_str()
    );
}

#[test]
fn the_service_refuses_what_the_corpus_marks_out_of_domain() {
    let corpus = corpus();
    for case in corpus["rejected"].as_array().expect("rejected") {
        let value: Value =
            serde_json::from_str(case["input"].as_str().expect("input")).expect("JSON");
        assert!(ExpectedValue::of_value(&value).is_err(), "{}", case["name"]);
    }
}
