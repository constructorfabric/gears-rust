use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{GtsPublishError, publish};
use crate::AdmissionFailureReason;
use crate::testing_platform::MockTypesRegistry;

const A: &str = "gts.cf.test.gts_publisher.a.v1~";

#[tokio::test]
async fn publishes_the_manifest_under_the_given_identity() {
    let registry = MockTypesRegistry::new();
    let documents = [(A.to_owned(), json!({"title": "a"}))];

    publish(
        &registry,
        "my-gear",
        "1.2.3",
        &documents,
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .expect("admitted");

    assert_eq!(registry.content(A), Some(json!({"title": "a"})));
    let submissions = registry.submissions();
    assert!(!submissions.is_empty());
    for (_, request) in submissions {
        assert_eq!(request.publisher.name, "my-gear");
        assert_eq!(request.publisher.version.to_string(), "1.2.3");
    }
}

#[tokio::test]
async fn a_rejection_is_a_terminal_publication_error() {
    let registry = MockTypesRegistry::new();
    registry.reject(A, AdmissionFailureReason::InvalidSchema);

    let err = publish(
        &registry,
        "my-gear",
        "1.0.0",
        &[(A.to_owned(), json!({}))],
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();

    assert!(matches!(err, GtsPublishError::Publication(_)), "{err}");
    assert!(err.to_string().contains(A), "{err}");
}

#[tokio::test]
async fn invalid_input_is_reported_before_any_request() {
    let registry = MockTypesRegistry::new();
    for (version, id) in [("not-semver", A), ("1.0.0", "not a gts id")] {
        let err = publish(
            &registry,
            "my-gear",
            version,
            &[(id.to_owned(), json!({}))],
            &CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(err, GtsPublishError::Input(_)), "{err}");
    }
    assert!(registry.submissions().is_empty());
}
