use super::*;
use crate::{ActivityContext, ActivityError, RetryPolicy};
use crate::{contracts::ActivityDefinition, runtime::ActivityInput};
use async_trait::async_trait;
use std::time::Duration;
struct Step;
#[async_trait]
impl ErasedActivity for Step {
    async fn execute(
        &self,
        _: ActivityContext,
        _: ActivityInput,
    ) -> Result<serde_json::Value, ActivityError> {
        Ok(serde_json::Value::Null)
    }
}
fn definition() -> ExecutionDefinition {
    crate::contracts::DefinitionParts {
        saga: false,
        name: "test.catalog.v1".into(),
        parallel_groups: vec![],
        activities: vec![ActivityDefinition {
            compensation: None,
            fanout: None,
            id: ActivityId("step".into()),
            timeout: Duration::from_secs(90),
            retry: RetryPolicy::default(),
            handler: Arc::new(Step),
        }],

        flow: crate::contracts::DataFlowSpec::independent(["step"]),
    }
    .build()
    .unwrap()
}
#[test]
fn metadata_can_create_journal_without_handlers() {
    let registry = Registry::default();
    let definition = definition();
    registry
        .register_contract(definition.contract().clone())
        .unwrap();
    registry.validate_bindings().unwrap();
    let contract = registry.get(definition.name()).unwrap();
    let journal = crate::state::journal::Journal::new(
        crate::RunId(uuid::Uuid::new_v4()),
        crate::ExecutionOwner {
            tenant_id: uuid::Uuid::new_v4(),
            subject_id: uuid::Uuid::new_v4(),
        },
        &contract,
        serde_json::Value::Null,
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(journal.run.activities.len(), 1);
    assert!(
        registry
            .handler(definition.name(), &definition.activities()[0].id)
            .is_err()
    );
    registry.validate_bindings().unwrap();
}
#[test]
fn worker_requires_matching_handlers_before_sealing() {
    let registry = Registry::default();
    let definition = definition();
    registry
        .register_contract(definition.contract().clone())
        .unwrap();
    registry.validate_bindings().unwrap();
    let incompatible = crate::test_support::edit(definition.clone(), |d| {
        d.activities[0].timeout += Duration::from_secs(1);
    });
    assert!(matches!(
        registry.register(incompatible),
        Err(DomainError::DefinitionConflict(_))
    ));
    assert!(
        registry
            .handler(definition.name(), &definition.activities()[0].id)
            .is_err()
    );
    registry.register(definition.clone()).unwrap();
    registry.validate_bindings().unwrap();
    assert!(
        registry
            .handler(definition.name(), &definition.activities()[0].id)
            .is_ok()
    );
    assert!(matches!(
        registry.register(definition),
        Err(DomainError::DefinitionConflict(_))
    ));
}
#[tokio::test]
async fn duplicate_binding_cannot_replace_live_handler() {
    struct Tagged(&'static str, Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait]
    impl ErasedActivity for Tagged {
        async fn execute(
            &self,
            _: ActivityContext,
            _: ActivityInput,
        ) -> Result<serde_json::Value, ActivityError> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(serde_json::json!(self.0))
        }
    }
    let registry = Registry::default();
    let original_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let replacement_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let original = crate::test_support::edit(definition(), |d| {
        d.activities[0].handler = Arc::new(Tagged("original", original_calls.clone()));
    });
    registry.register(original.clone()).unwrap();
    let replacement = crate::test_support::edit(original.clone(), |d| {
        d.activities[0].handler = Arc::new(Tagged("replacement", replacement_calls.clone()));
    });
    assert!(matches!(
        registry.register(replacement),
        Err(DomainError::DefinitionConflict(_))
    ));
    assert!(matches!(
        registry.register_contract(original.contract().clone()),
        Err(DomainError::DefinitionConflict(_))
    ));
    registry.validate_bindings().unwrap();
    let handler = registry
        .handler(original.name(), &original.activities()[0].id)
        .unwrap();
    let result = handler
        .execute(
            ActivityContext::from(crate::activity::ActivityContextInit {
                instance: crate::ActivityInstanceId {
                    step: original.activities()[0].id.clone(),
                    item_index: None,
                    phase: crate::ExecutionPhase::Forward,
                },
                run_id: crate::RunId(uuid::Uuid::new_v4()),
                activity_id: original.activities()[0].id.clone(),
                attempt: 1,
                execution_epoch: 0,
                idempotency_key: "fixture".into(),
                owner: crate::ExecutionOwner {
                    tenant_id: uuid::Uuid::new_v4(),
                    subject_id: uuid::Uuid::new_v4(),
                },
                deadline: chrono::Utc::now() + chrono::Duration::seconds(90),
                cancellation: tokio_util::sync::CancellationToken::new(),
            }),
            ActivityInput {
                selected_input: None,
                run_input: serde_json::Value::Null,
                previous_results: BTreeMap::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result, serde_json::json!("original"));
    assert_eq!(original_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        replacement_calls.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}
#[test]
fn contract_round_trip_preserves_new_fingerprint() {
    let definition = definition();
    let contract = definition.contract();
    let decoded: ExecutionContract =
        serde_json::from_slice(&serde_json::to_vec(&contract).unwrap()).unwrap();
    assert_eq!(*contract, decoded);
    // Fixed SHA-256 vector from the original RustCrypto/legacy serialization:
    // ["test.catalog.v1",[["step",90000,{"max_attempts":10,"initial_delay_secs":15,"max_delay_secs":300}]]]
    assert_eq!(
        decoded.fingerprint().unwrap(),
        "fd7c995c44d47da8449aea0e1260c0fcb06050010cf4dcf4668af16e3c7eb05b"
    );
}
