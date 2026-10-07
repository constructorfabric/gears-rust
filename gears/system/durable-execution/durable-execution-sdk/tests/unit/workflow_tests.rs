use super::*;
use crate::ExecutionOwner;
use chrono::Utc;
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;
fn step<I: Payload, O: Payload, F, Fut>(id: &str, f: F) -> Step<I, O>
where
    F: Fn(StepContext, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O, ActivityError>> + Send + 'static,
{
    Step::new(id, f).timeout(Duration::from_secs(10))
}
fn context(id: &str) -> ActivityContext {
    ActivityContext {
        run_id: crate::RunId(uuid::Uuid::new_v4()),
        activity_id: ActivityId(id.into()),
        attempt: 1,
        execution_epoch: 0,
        idempotency_key: id.into(),
        owner: ExecutionOwner {
            tenant_id: uuid::Uuid::new_v4(),
            subject_id: uuid::Uuid::new_v4(),
        },
        deadline: Utc::now(),
        cancellation: CancellationToken::new(),
    }
}
async fn execute(
    definition: &ExecutionDefinition,
    id: &str,
    input: Value,
    previous: &BTreeMap<String, Value>,
) -> Result<Value, ActivityError> {
    definition
        .activities
        .iter()
        .find(|a| a.id.0 == id)
        .unwrap()
        .handler
        .execute(
            context(id),
            ActivityInput {
                run_input: input,
                previous_results: previous.clone(),
            },
        )
        .await
}
#[tokio::test]
async fn chain_reads_serialized_checkpoint_and_declared_early_result() {
    let builder = WorkflowBuilder::<u32>::new("typed.chain.v1")
        .then(step("prepare", |_, n: u32| async move { Ok(n + 1) }));
    let prepared = builder.checkpoint();
    let reference = prepared.clone();
    let workflow = builder
        .then(step("format", |_, n: u32| async move { Ok(n.to_string()) }))
        .then(
            step("combine", move |ctx, text: String| {
                let prepared = prepared.clone();
                async move { Ok(format!("{text}:{}", ctx.checkpoint(&prepared)?)) }
            })
            .uses(&reference),
        )
        .build()
        .unwrap();
    let contract = workflow.contract();
    let wire = serde_json::to_vec(&contract).unwrap();
    assert_eq!(
        serde_json::from_slice::<ExecutionContract>(&wire).unwrap(),
        contract
    );
    let definition = workflow.into_definition();
    let mut results = BTreeMap::new();
    for id in ["prepare", "format", "combine"] {
        let value = execute(&definition, id, Value::from(4), &results)
            .await
            .unwrap();
        results.insert(id.into(), value);
    }
    assert_eq!(results["combine"], Value::from("5:5"));
    let restored =
        serde_json::from_slice::<BTreeMap<String, Value>>(&serde_json::to_vec(&results).unwrap())
            .unwrap();
    assert_eq!(
        execute(&definition, "combine", Value::from(4), &restored)
            .await
            .unwrap(),
        Value::from("5:5")
    );
}
#[tokio::test]
async fn heterogeneous_parallel_outputs_are_ordered_before_join() {
    let workflow = WorkflowBuilder::<u32>::new("typed.parallel.v1")
        .parallel((
            step("number", |_, n: u32| async move { Ok(n + 1) }),
            step("text", |_, n: u32| async move { Ok(n.to_string()) }),
        ))
        .then(step(
            "join",
            |_, (number, text): (u32, String)| async move { Ok(format!("{number}:{text}")) },
        ))
        .build()
        .unwrap();
    let definition = workflow.into_definition();
    assert_eq!(
        definition.stages().unwrap(),
        vec![vec!["number", "text"], vec!["join"]]
    );
    let mut results = BTreeMap::new();
    for id in ["text", "number"] {
        let value = execute(&definition, id, Value::from(3), &results)
            .await
            .unwrap();
        results.insert(id.into(), value);
    }
    assert_eq!(
        execute(&definition, "join", Value::from(3), &results)
            .await
            .unwrap(),
        Value::from("4:3")
    );
}
#[tokio::test]
async fn dynamic_parallel_preserves_declaration_order_and_single_branch_shape() {
    for count in [1, 3] {
        let branches = (0..count)
            .map(|offset| {
                step(&format!("branch-{offset}"), move |_, n: u32| async move {
                    Ok(n + offset)
                })
            })
            .collect::<Vec<_>>();
        let workflow = WorkflowBuilder::<u32>::new("typed.dynamic.v1")
            .parallel(branches)
            .then(step("sum", |_, items: Vec<u32>| async move { Ok(items) }))
            .build()
            .unwrap();
        let definition = workflow.into_definition();
        let mut results = BTreeMap::new();
        for offset in (0..count).rev() {
            let id = format!("branch-{offset}");
            let value = execute(&definition, &id, Value::from(10), &results)
                .await
                .unwrap();
            results.insert(id, value);
        }
        assert_eq!(
            execute(&definition, "sum", Value::from(10), &results)
                .await
                .unwrap(),
            serde_json::to_value((0..count).map(|n| n + 10).collect::<Vec<_>>()).unwrap()
        );
    }
}
#[test]
fn invalid_policy_duplicate_and_foreign_reference_fail_before_registration() {
    assert!(
        WorkflowBuilder::<u32>::new("typed.invalid.v1")
            .then(Step::new("no-timeout", |_, n: u32| async move { Ok(n) }))
            .build()
            .is_err()
    );
    assert!(
        WorkflowBuilder::<u32>::new("typed.invalid.v1")
            .parallel(Vec::<Step<u32, u32>>::new())
            .build()
            .is_err()
    );
    assert!(
        WorkflowBuilder::<u32>::new("typed.invalid.v1")
            .then(step("same", |_, n: u32| async move { Ok(n) }))
            .then(step("same", |_, n: u32| async move { Ok(n) }))
            .build()
            .is_err()
    );
    let empty = WorkflowBuilder::<u32>::new("typed.invalid.v1");
    let unsaved = empty.checkpoint();
    assert!(
        empty
            .then(step("first", |_, n: u32| async move { Ok(n) }).uses(&unsaved))
            .build()
            .is_err()
    );
    let foreign = WorkflowBuilder::<u32>::new("typed.other.v1")
        .then(step("first", |_, n: u32| async move { Ok(n) }))
        .checkpoint();
    assert!(
        WorkflowBuilder::<u32>::new("typed.invalid.v1")
            .then(step("first", |_, n: u32| async move { Ok(n) }))
            .then(step("last", |_, n: u32| async move { Ok(n) }).uses(&foreign))
            .build()
            .is_err()
    );
}
#[test]
fn serialized_sources_reject_current_and_future_parallel_dependencies_and_change_fingerprint() {
    let mut contract = WorkflowBuilder::<u32>::new("typed.sources.v1")
        .parallel((
            step("a", |_, n: u32| async move { Ok(n) }),
            step("b", |_, n: u32| async move { Ok(n) }),
        ))
        .then(step("c", |_, n: (u32, u32)| async move { Ok(n.0) }))
        .build()
        .unwrap()
        .contract();
    let original = contract.fingerprint().unwrap();
    contract
        .flow
        .as_mut()
        .unwrap()
        .inputs
        .insert("a".into(), InputSource::Checkpoint("b".into()));
    assert!(contract.validate().is_err());
    contract
        .flow
        .as_mut()
        .unwrap()
        .inputs
        .insert("a".into(), InputSource::Checkpoint("c".into()));
    assert!(contract.validate().is_err());
    contract
        .flow
        .as_mut()
        .unwrap()
        .inputs
        .insert("a".into(), InputSource::RunInput);
    contract
        .flow
        .as_mut()
        .unwrap()
        .inputs
        .insert("c".into(), InputSource::Checkpoint("a".into()));
    assert_ne!(original, contract.fingerprint().unwrap());
}
#[tokio::test]
async fn undeclared_checkpoint_and_bad_payload_are_safe_failures() {
    let builder = WorkflowBuilder::<u32>::new("typed.decode.v1")
        .then(step("a", |_, n: u32| async move { Ok(n) }));
    let checkpoint = builder.checkpoint();
    let workflow = builder
        .then(step("b", move |ctx, n: u32| {
            let checkpoint = checkpoint.clone();
            async move { ctx.checkpoint(&checkpoint).map(|_: u32| n) }
        }))
        .build()
        .unwrap();
    let definition = workflow.into_definition();
    assert_eq!(
        execute(
            &definition,
            "a",
            Value::from("secret-value"),
            &BTreeMap::new()
        )
        .await
        .unwrap_err()
        .code(),
        "activity_input_decode_failed"
    );
    assert_eq!(
        execute(
            &definition,
            "b",
            Value::from(1),
            &BTreeMap::from([("a".into(), Value::from(1))])
        )
        .await
        .unwrap_err()
        .code(),
        "undeclared_checkpoint"
    );
}

#[tokio::test]
async fn typed_branch_references_read_individual_checkpoints_and_preserve_dependencies() {
    let builder = WorkflowBuilder::<u32>::new("typed.branch-refs.v1").parallel((
        step("number", |_, n: u32| async move { Ok(n + 1) }),
        step("text", |_, n: u32| async move { Ok(n.to_string()) }),
    ));
    let (number, text) = builder.checkpoint().branches().unwrap();
    let number_dependency = number.clone();
    let text_dependency = text.clone();
    let definition = builder
        .then(
            step("join", move |ctx, _: (u32, String)| {
                let number = number.clone();
                let text = text.clone();
                async move {
                    Ok(format!(
                        "{}:{}",
                        ctx.checkpoint(&number)?,
                        ctx.checkpoint(&text)?
                    ))
                }
            })
            .uses(&number_dependency)
            .uses(&text_dependency),
        )
        .build()
        .unwrap()
        .into_definition();
    let results = [
        ("number".into(), Value::from(5)),
        ("text".into(), Value::from("4")),
    ]
    .into();
    assert_eq!(
        execute(&definition, "join", Value::from(4), &results)
            .await
            .unwrap(),
        Value::from("5:4")
    );
    let builder = WorkflowBuilder::<u32>::new("typed.list-refs.v1").parallel(vec![
        step("a", |_, n: u32| async move { Ok(n) }),
        step("b", |_, n: u32| async move { Ok(n) }),
    ]);
    let branches = builder.checkpoint().branches().unwrap();
    assert_eq!(
        branches.iter().map(StepRef::source).collect::<Vec<_>>(),
        vec![
            &InputSource::Checkpoint("a".into()),
            &InputSource::Checkpoint("b".into())
        ]
    );
    assert!(
        WorkflowBuilder::<u32>::new("typed.sequential.v1")
            .then(step("tuple", |_, n: u32| async move { Ok((n, n)) }))
            .checkpoint()
            .branches()
            .is_none()
    );
    assert!(
        WorkflowBuilder::<u32>::new("typed.sequential.v1")
            .then(step("list", |_, n: u32| async move { Ok(vec![n]) }))
            .checkpoint()
            .branches()
            .is_none()
    );
}
