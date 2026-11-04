use super::*;
use crate::ExecutionOwner;
use crate::activity::ActivityContextInit;
use chrono::Utc;
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;
fn step<I: Payload, O: Payload, F, Fut>(id: &str, f: F) -> Step<I, O>
where
    F: Fn(ActivityContext, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O, ActivityError>> + Send + 'static,
{
    Step::new(id, Duration::from_secs(10), f)
}
fn context(id: &str) -> ActivityContext {
    ActivityContext::from(ActivityContextInit {
        instance: crate::ActivityInstanceId {
            step: ActivityId(id.into()),
            item_index: None,
            phase: crate::ExecutionPhase::Forward,
        },
        run_id: RunId(uuid::Uuid::new_v4()),
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
    })
}
async fn execute(
    definition: &ExecutionDefinition,
    id: &str,
    input: Value,
    previous: &BTreeMap<String, Value>,
) -> Result<Value, ActivityError> {
    definition
        .activities()
        .iter()
        .find(|a| a.id.0 == id)
        .unwrap()
        .handler
        .execute(
            context(id),
            ActivityInput {
                selected_input: None,
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
    let workflow = builder
        .then(step("format", |_, n: u32| async move { Ok(n.to_string()) }))
        .then(Step::with_deps(
            "combine",
            Duration::from_secs(10),
            &prepared,
            |_, text: String, prepared: u32| async move { Ok(format!("{text}:{prepared}")) },
        ))
        .build()
        .unwrap();
    let contract = workflow.contract().clone();
    let wire = serde_json::to_vec(&contract).unwrap();
    assert_eq!(
        serde_json::from_slice::<ExecutionContract>(&wire).unwrap(),
        contract
    );
    assert_eq!(
        contract.flow.dependencies["combine"],
        vec![InputSource::Checkpoint("prepare".into())]
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
        definition.stages(),
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
fn build_error<I: Payload, O: Payload, K>(
    builder: WorkflowBuilder<I, O, K>,
) -> (DefinitionErrorKind, Option<String>) {
    let error = builder.build().unwrap_err();
    (error.kind(), error.step().map(str::to_owned))
}
#[test]
fn invalid_definitions_report_the_rule_and_the_step() {
    assert_eq!(
        build_error(
            WorkflowBuilder::<u32>::new("typed.invalid.v1").then(Step::new(
                "no-timeout",
                Duration::ZERO,
                |_, n: u32| async move { Ok(n) }
            ))
        ),
        (
            DefinitionErrorKind::InvalidTimeout,
            Some("no-timeout".into())
        )
    );
    assert_eq!(
        build_error(
            WorkflowBuilder::<u32>::new("typed.invalid.v1").parallel(Vec::<Step<u32, u32>>::new())
        ),
        (DefinitionErrorKind::InvalidParallelStage, None)
    );
    assert_eq!(
        build_error(
            WorkflowBuilder::<u32>::new("typed.invalid.v1")
                .then(step("same", |_, n: u32| async move { Ok(n) }))
                .then(step("same", |_, n: u32| async move { Ok(n) }))
        ),
        (
            DefinitionErrorKind::DuplicateActivityId,
            Some("same".into())
        )
    );
    assert_eq!(
        build_error(WorkflowBuilder::<u32>::new("typed.invalid.v1").then(
            step("first", |_, n: u32| async move { Ok(n) }).retry(RetryPolicy::exponential(
                0,
                Duration::from_secs(1),
                Duration::from_secs(1)
            ))
        )),
        (
            DefinitionErrorKind::InvalidRetryPolicy,
            Some("first".into())
        )
    );
    let empty = WorkflowBuilder::<u32>::new("typed.invalid.v1");
    let run_input = empty.checkpoint();
    assert_eq!(
        build_error(empty.then(Step::with_deps(
            "first",
            Duration::from_secs(1),
            &run_input,
            |_, n: u32, _: u32| async move { Ok(n) }
        ))),
        (DefinitionErrorKind::InvalidDependency, Some("first".into()))
    );
    let foreign = WorkflowBuilder::<u32>::new("typed.other.v1")
        .then(step("first", |_, n: u32| async move { Ok(n) }))
        .checkpoint();
    assert_eq!(
        build_error(
            WorkflowBuilder::<u32>::new("typed.invalid.v1")
                .then(step("first", |_, n: u32| async move { Ok(n) }))
                .then(Step::with_deps(
                    "last",
                    Duration::from_secs(1),
                    &foreign,
                    |_, n: u32, _: u32| async move { Ok(n) }
                ))
        ),
        (DefinitionErrorKind::InvalidDependency, Some("last".into()))
    );
    let error = WorkflowBuilder::<u32>::new("Typed.Invalid")
        .then(step("first", |_, n: u32| async move { Ok(n) }))
        .build()
        .unwrap_err();
    assert_eq!(error.kind(), DefinitionErrorKind::InvalidName);
    assert!(
        error
            .to_string()
            .starts_with("invalid execution definition: name must")
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
        .contract()
        .clone();
    let original = contract.fingerprint().unwrap();
    let flow = |contract: &mut ExecutionContract| contract.flow.inputs.clone();
    let mut inputs = flow(&mut contract);
    inputs.insert("a".into(), InputSource::Checkpoint("b".into()));
    contract.flow.inputs = inputs.clone();
    let error = contract.validate().unwrap_err();
    assert_eq!(
        (error.kind(), error.step()),
        (DefinitionErrorKind::InvalidDataFlow, Some("a"))
    );
    inputs.insert("a".into(), InputSource::Checkpoint("c".into()));
    contract.flow.inputs = inputs.clone();
    assert!(contract.validate().is_err());
    inputs.insert("a".into(), InputSource::RunInput);
    inputs.insert("c".into(), InputSource::Checkpoint("a".into()));
    contract.flow.inputs = inputs;
    assert_ne!(original, contract.fingerprint().unwrap());
}
#[tokio::test]
async fn missing_or_undecodable_inputs_are_safe_failures() {
    let builder = WorkflowBuilder::<u32>::new("typed.decode.v1")
        .then(step("a", |_, n: u32| async move { Ok(n) }));
    let checkpoint = builder.checkpoint();
    let definition = builder
        .then(Step::with_deps(
            "b",
            Duration::from_secs(1),
            &checkpoint,
            |_, n: u32, a: u32| async move { Ok(n + a) },
        ))
        .build()
        .unwrap()
        .into_definition();
    let code = |result: Result<Value, ActivityError>| result.unwrap_err().code().to_owned();
    assert_eq!(
        code(
            execute(
                &definition,
                "a",
                Value::from("secret-value"),
                &BTreeMap::new()
            )
            .await
        ),
        "activity_input_decode_failed"
    );
    assert_eq!(
        code(execute(&definition, "b", Value::from(1), &BTreeMap::new()).await),
        "checkpoint_missing"
    );
    let undecodable = BTreeMap::from([("a".into(), Value::from("secret-value"))]);
    assert_eq!(
        code(execute(&definition, "b", Value::from(1), &undecodable).await),
        "activity_input_decode_failed"
    );
    let builder = WorkflowBuilder::<u32>::new("typed.decode.v1")
        .then(step("a", |_, n: u32| async move { Ok(n) }));
    let a = builder.checkpoint();
    let definition = builder
        .then(step("text", |_, n: u32| async move { Ok(n.to_string()) }))
        .then(Step::with_deps(
            "c",
            Duration::from_secs(1),
            &a,
            |_, s: String, a: u32| async move { Ok(format!("{s}{a}")) },
        ))
        .build()
        .unwrap()
        .into_definition();
    let results = BTreeMap::from([
        ("a".into(), Value::from("corrupted")),
        ("text".into(), Value::from("7")),
    ]);
    assert_eq!(
        code(execute(&definition, "c", Value::from(1), &results).await),
        "checkpoint_decode_failed"
    );
}

#[tokio::test]
async fn typed_branch_references_read_individual_checkpoints_and_preserve_dependencies() {
    let builder = WorkflowBuilder::<u32>::new("typed.branch-refs.v1").parallel((
        step("number", |_, n: u32| async move { Ok(n + 1) }),
        step("text", |_, n: u32| async move { Ok(n.to_string()) }),
    ));
    let (number, text) = builder.checkpoint().branches();
    let definition = builder
        .then(Step::with_deps(
            "join",
            Duration::from_secs(1),
            (&number, &text),
            |_, _: (u32, String), (number, text): (u32, String)| async move {
                Ok(format!("{number}:{text}"))
            },
        ))
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
    let branches = builder.checkpoint().branches();
    assert_eq!(
        branches.iter().map(StepRef::source).collect::<Vec<_>>(),
        vec![
            &InputSource::Checkpoint("a".into()),
            &InputSource::Checkpoint("b".into())
        ]
    );
}

#[test]
fn references_share_the_cached_fingerprint() {
    let builder = WorkflowBuilder::<u32>::new("typed.fingerprint.v1")
        .then(step("a", |_, n: u32| async move { Ok(n) }));
    let checkpoint = builder.checkpoint();
    assert_eq!(
        checkpoint.fingerprint().unwrap_err().kind(),
        DefinitionErrorKind::NotFinalized
    );
    let workflow = builder.build().unwrap();
    let reference = workflow.reference();
    assert_eq!(
        reference.fingerprint(),
        workflow.contract().fingerprint().unwrap()
    );
    assert_eq!(checkpoint.fingerprint().unwrap(), reference.fingerprint());
    let restored = WorkflowRef::<u32, u32>::from_contract(workflow.contract().clone()).unwrap();
    assert_eq!(restored.fingerprint(), reference.fingerprint());
    let (parts_ref, definition) = workflow.into_parts();
    assert_eq!(parts_ref.name(), definition.name());
}
