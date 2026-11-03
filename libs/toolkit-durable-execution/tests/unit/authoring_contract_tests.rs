use crate::{DefinitionErrorKind, prelude::*, runtime::ActivityInput};
use std::time::Duration;

#[test]
fn captured_references_finalize_only_after_valid_build_and_policy_changes_change_identity() {
    let builder = WorkflowBuilder::<u32>::new("authoring.identity.v1").then(Step::new(
        "one",
        Duration::from_secs(1),
        |_, n: u32| async move { Ok(n) },
    ));
    let reference = builder.checkpoint();
    assert!(reference.fingerprint().is_err());
    let workflow = builder.build().unwrap();
    assert_eq!(
        reference.fingerprint().unwrap(),
        workflow.contract().fingerprint().unwrap()
    );
    let failed = WorkflowBuilder::<u32>::new("authoring.invalid.v1").then(Step::new(
        "one",
        Duration::ZERO,
        |_, n: u32| async move { Ok(n) },
    ));
    let failed_reference = failed.checkpoint();
    assert_eq!(
        failed.build().unwrap_err().kind(),
        DefinitionErrorKind::InvalidTimeout
    );
    assert_eq!(
        failed_reference.fingerprint().unwrap_err().kind(),
        DefinitionErrorKind::NotFinalized
    );
    let mut contract = workflow.contract().clone();
    contract.saga = true;
    assert_ne!(
        reference.fingerprint().unwrap(),
        contract.fingerprint().unwrap()
    );
}

#[test]
fn authoring_and_error_debug_never_format_handler_captures_or_payloads() {
    let credential = "provider-secret-value".to_owned();
    let builder = WorkflowBuilder::<String>::new("authoring.debug.v1").then(Step::new(
        "one",
        Duration::from_secs(1),
        move |_, input: String| {
            let credential = credential.clone();
            async move { Ok(format!("{credential}{input}")) }
        },
    ));
    assert!(!format!("{builder:?}").contains("provider-secret-value"));
    let input = ActivityInput {
        selected_input: Some(serde_json::json!("private-payload")),
        run_input: serde_json::json!("private-payload"),
        previous_results: [("one".into(), serde_json::json!("private-payload"))].into(),
    };
    assert!(!format!("{input:?}").contains("private-payload"));
    let error = ActivityError::retryable_after("credential=secret\n", Duration::from_secs(2));
    assert!(!format!("{error:?} {error}").contains("secret"));
    let _: &dyn std::error::Error = &error;
}
