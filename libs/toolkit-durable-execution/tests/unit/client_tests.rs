use super::*;
use crate::observation::{RecordedTime, RunState};
use crate::{ErrorKind, EventKind, ExecutionOwner};
use chrono::Utc;
use futures_util::StreamExt;
use std::sync::Mutex;

/// Serves scripted progress snapshots; each `events` call reveals the next one.
struct Scripted {
    states: Vec<RunState>,
    position: Mutex<usize>,
    fail_events: bool,
}
impl Scripted {
    fn new(states: Vec<RunState>, fail_events: bool) -> Arc<Self> {
        Arc::new(Self {
            states,
            position: Mutex::new(0),
            fail_events,
        })
    }
    fn progress(&self, id: RunId, position: usize) -> RunProgress {
        RunProgress {
            id,
            definition: "test.watch.v1".into(),
            owner: ExecutionOwner {
                tenant_id: uuid::Uuid::nil(),
                subject_id: uuid::Uuid::nil(),
            },
            epoch: 0,
            cursor: u64::try_from(position).unwrap(),
            state: self.states[position].clone(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            steps: vec![],
        }
    }
}
fn unused() -> Error {
    crate::error::domain::DomainError::Internal("not used by this test").into()
}
#[async_trait]
impl RunService for Scripted {
    async fn start(
        &self,
        _: &SecurityContext,
        _: &str,
        _: Value,
        _: StartOptions,
    ) -> Result<StartResult, Error> {
        Err(unused())
    }
    async fn get(&self, _: &SecurityContext, id: RunId) -> Result<RunProgress, Error> {
        let position = *self.position.lock().unwrap();
        Ok(self.progress(id, position))
    }
    async fn events(
        &self,
        _: &SecurityContext,
        id: RunId,
        after: u64,
        _: u32,
    ) -> Result<Vec<RunEvent>, Error> {
        if self.fail_events {
            return Err(crate::error::domain::DomainError::Unavailable.into());
        }
        let mut position = self.position.lock().unwrap();
        if *position + 1 >= self.states.len() {
            return Ok(vec![]);
        }
        *position += 1;
        Ok(vec![RunEvent {
            instance: None,
            sequence: after + 1,
            run_id: id,
            activity_id: None,
            kind: EventKind::Completed,
            epoch: 0,
            attempt: None,
            at: Utc::now(),
            error_code: None,
        }])
    }
    async fn history(&self, _: &SecurityContext, _: RunId) -> Result<RunHistory, Error> {
        Err(unused())
    }
    async fn result(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: &str,
        _: &str,
        _: ResultSelector,
    ) -> Result<Value, Error> {
        Err(unused())
    }
    async fn abort(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: AbortOptions,
    ) -> Result<CancelResult, Error> {
        Err(unused())
    }
    async fn cancel(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: CancelOptions,
    ) -> Result<CancelResult, Error> {
        Err(unused())
    }
    async fn retry(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: ContinueOptions,
    ) -> Result<RunProgress, Error> {
        Err(unused())
    }
    async fn resume(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: ContinueOptions,
    ) -> Result<RunProgress, Error> {
        Err(unused())
    }
}
fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid::Uuid::new_v4())
        .subject_tenant_id(uuid::Uuid::new_v4())
        .subject_type("user")
        .build()
        .unwrap()
}
fn lifecycle() -> Vec<RunState> {
    vec![
        RunState::Queued,
        RunState::Running,
        RunState::Succeeded {
            completed_at: RecordedTime::Unknown,
        },
    ]
}

#[tokio::test]
async fn watch_streams_each_committed_change_and_ends_when_settled() {
    let client = ExecutionClient::new(Scripted::new(lifecycle(), false))
        .with_poll_interval(Duration::from_millis(1));
    let ctx = ctx();
    let id = RunId::from(uuid::Uuid::new_v4());
    let seen: Vec<_> = client
        .watch(&ctx, id)
        .map(|progress| progress.unwrap().cursor)
        .collect()
        .await;
    assert_eq!(seen, [0, 1, 2]);
    let settled = client.wait(&ctx, id).await.unwrap();
    assert!(settled.state.is_settled());
}

#[tokio::test]
async fn watch_ends_after_the_first_error() {
    let client = ExecutionClient::new(Scripted::new(lifecycle(), true));
    let ctx = ctx();
    let items: Vec<_> = client
        .watch(&ctx, RunId::from(uuid::Uuid::new_v4()))
        .collect()
        .await;
    assert_eq!(items.len(), 2);
    assert!(items[0].is_ok());
    assert!(items[1].is_err());
}

/// Answers inspection with fixed progress and lists nothing.
struct FakeInspector(Arc<Scripted>);
#[async_trait]
impl InspectService for FakeInspector {
    async fn inspect(&self, _: &SecurityContext, id: RunId) -> Result<RunProgress, Error> {
        Ok(self.0.progress(id, 0))
    }
    async fn list(&self, _: &SecurityContext, _: WorkflowQuery) -> Result<WorkflowPage, Error> {
        Err(unused())
    }
}

#[tokio::test]
async fn consumer_tests_can_drive_handles_over_fake_services() {
    let scripted = Scripted::new(lifecycle(), false);
    let client = ExecutionClient::from_service(scripted.clone())
        .with_poll_interval(Duration::from_millis(1));
    let inspector = ExecutionInspector::from_service(Arc::new(FakeInspector(scripted)));
    let ctx = ctx();
    let id = RunId::from(uuid::Uuid::new_v4());
    assert_eq!(client.get(&ctx, id).await.unwrap().id, id);
    assert_eq!(inspector.inspect(&ctx, id).await.unwrap().id, id);
    let error = inspector
        .list(&ctx, WorkflowQuery::new(Utc::now()))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Internal);
}

/// A run service that records what the typed client sends it.
struct Recording {
    starts: Mutex<Vec<(String, Value, StartOptions)>>,
    selectors: Mutex<Vec<ResultSelector>>,
    controls: Mutex<Vec<&'static str>>,
    result: Value,
    reads: std::sync::atomic::AtomicUsize,
    /// `get` reports `Running` until this many reads have happened.
    settle_after: usize,
    /// `get` fails on this read (1-based).
    fail_read: Option<usize>,
}

impl Recording {
    fn new(result: Value) -> Arc<Self> {
        Self::reading(result, 1, None)
    }
    fn reading(result: Value, settle_after: usize, fail_read: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            starts: Mutex::default(),
            selectors: Mutex::default(),
            controls: Mutex::default(),
            result,
            reads: std::sync::atomic::AtomicUsize::new(0),
            settle_after,
            fail_read,
        })
    }
    fn control(&self, name: &'static str) {
        self.controls.lock().unwrap().push(name);
    }
    fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn progress(id: RunId, state: RunState) -> RunProgress {
    RunProgress {
        id,
        definition: "client.typed.v1".into(),
        owner: ExecutionOwner {
            tenant_id: uuid::Uuid::nil(),
            subject_id: uuid::Uuid::nil(),
        },
        epoch: 0,
        cursor: 0,
        state,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        steps: vec![],
    }
}

#[async_trait]
impl RunService for Recording {
    async fn start(
        &self,
        _: &SecurityContext,
        definition: &str,
        input: Value,
        options: StartOptions,
    ) -> Result<StartResult, Error> {
        self.starts
            .lock()
            .unwrap()
            .push((definition.to_owned(), input, options));
        Ok(StartResult {
            run_id: RunId::from(uuid::Uuid::from_u128(42)),
            requested_generation: 3,
            coalesced: true,
        })
    }
    async fn get(&self, _: &SecurityContext, id: RunId) -> Result<RunProgress, Error> {
        let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if self.fail_read == Some(read) {
            return Err(crate::error::domain::DomainError::Unavailable.into());
        }
        Ok(progress(
            id,
            if read >= self.settle_after {
                RunState::Succeeded {
                    completed_at: RecordedTime::Unknown,
                }
            } else {
                RunState::Running
            },
        ))
    }
    async fn events(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: u64,
        _: u32,
    ) -> Result<Vec<RunEvent>, Error> {
        self.control("events");
        Ok(vec![])
    }
    async fn history(&self, _: &SecurityContext, _: RunId) -> Result<RunHistory, Error> {
        Err(unused())
    }
    async fn result(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: &str,
        _: &str,
        selector: ResultSelector,
    ) -> Result<Value, Error> {
        self.selectors.lock().unwrap().push(selector);
        Ok(self.result.clone())
    }
    async fn abort(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: AbortOptions,
    ) -> Result<CancelResult, Error> {
        self.control("abort");
        Ok(CancelResult::Requested)
    }
    async fn cancel(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: CancelOptions,
    ) -> Result<CancelResult, Error> {
        self.control("cancel");
        Ok(CancelResult::Unchanged)
    }
    async fn retry(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        _: ContinueOptions,
    ) -> Result<RunProgress, Error> {
        self.control("retry");
        self.get(ctx, id).await
    }
    async fn resume(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        _: ContinueOptions,
    ) -> Result<RunProgress, Error> {
        self.control("resume");
        self.get(ctx, id).await
    }
}

/// A one-step workflow whose output doubles the input, and a checkpoint of it.
fn doubling(
    name: &str,
) -> (
    crate::workflow::Workflow<u32, u32>,
    crate::workflow::StepRef<u32>,
) {
    let builder = crate::workflow::WorkflowBuilder::<u32>::new(name).then(
        crate::workflow::Step::new("double", Duration::from_secs(5), |_, n: u32| async move {
            Ok(n * 2)
        }),
    );
    let checkpoint = builder.checkpoint();
    (builder.build().unwrap(), checkpoint)
}

#[tokio::test]
async fn start_pins_the_workflow_fingerprint_and_returns_a_typed_handle() {
    let service = Recording::new(Value::Null);
    let client = ExecutionClient::new(service.clone());
    let (workflow, _) = doubling("client.typed.v1");
    let reference = workflow.reference();
    let started = client
        .start(
            &ctx(),
            &reference,
            21,
            StartOptions::new().idempotency_key("k"),
        )
        .await
        .unwrap();
    assert_eq!(started.run_id(), RunId::from(uuid::Uuid::from_u128(42)));
    assert_eq!(started.requested_generation(), 3);
    assert!(started.coalesced());
    assert_eq!(started.handle().id(), started.run_id());
    assert_eq!(
        started.handle().workflow().fingerprint(),
        reference.fingerprint()
    );
    // A matching explicit fingerprint is accepted too.
    let again = client
        .start(
            &ctx(),
            &reference,
            1,
            StartOptions::new().expected_fingerprint(reference.fingerprint()),
        )
        .await
        .unwrap();
    assert_eq!(again.run_id(), started.run_id());
    let starts = service.starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2);
    let (name, input, options) = &starts[0];
    assert_eq!(name, "client.typed.v1");
    assert_eq!(input, &serde_json::json!(21));
    assert_eq!(options.idempotency_key.as_deref(), Some("k"));
    assert_eq!(
        options.expected_fingerprint.as_deref(),
        Some(reference.fingerprint())
    );
}

#[tokio::test]
async fn start_rejects_a_conflicting_fingerprint_before_reaching_the_service() {
    let service = Recording::new(Value::Null);
    let client = ExecutionClient::new(service.clone());
    let (workflow, _) = doubling("client.typed.v1");
    let error = client
        .start(
            &ctx(),
            &workflow.reference(),
            21,
            StartOptions::new().expected_fingerprint("another-contract"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    assert_eq!(error.field().as_deref(), Some("expected_fingerprint"));
    assert!(service.starts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn results_decode_as_the_typed_output_or_fail_as_internal() {
    let (workflow, checkpoint) = doubling("client.typed.v1");
    let handle = workflow.reference().attach(RunId::from(uuid::Uuid::nil()));

    let service = Recording::new(serde_json::json!(42));
    let client = ExecutionClient::new(service.clone());
    assert_eq!(client.result(&ctx(), &handle).await.unwrap(), 42);
    assert_eq!(
        client
            .step_result(&ctx(), &handle, &checkpoint)
            .await
            .unwrap(),
        42
    );
    let selectors = service.selectors.lock().unwrap().clone();
    assert_eq!(selectors[0], ResultSelector::Workflow);
    assert!(matches!(selectors[1], ResultSelector::Checkpoint(_)));

    let wrong = ExecutionClient::new(Recording::new(serde_json::json!("not a number")));
    let error = wrong.result(&ctx(), &handle).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Internal);
    assert!(error.to_string().contains("workflow result"));
    assert!(std::error::Error::source(&error).is_some());
    let error = wrong
        .step_result(&ctx(), &handle, &checkpoint)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("checkpoint"));
}

#[tokio::test]
async fn step_result_rejects_a_checkpoint_of_another_workflow_before_reading() {
    let service = Recording::new(serde_json::json!(42));
    let client = ExecutionClient::new(service.clone());
    let (workflow, _) = doubling("client.typed.v1");
    let (_, foreign) = doubling("client.other.v1");
    let handle = workflow.reference().attach(RunId::from(uuid::Uuid::nil()));
    let error = client
        .step_result(&ctx(), &handle, &foreign)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::FailedPrecondition);
    assert_eq!(error.code(), crate::codes::DEFINITION_MISMATCH);
    assert!(service.selectors.lock().unwrap().is_empty());
}

#[tokio::test]
async fn wait_polls_until_the_run_settles() {
    let service = Recording::reading(Value::Null, 3, None);
    let client = ExecutionClient::new(service.clone()).with_poll_interval(Duration::from_millis(1));
    let id = RunId::from(uuid::Uuid::new_v4());
    let settled = client.wait(&ctx(), id).await.unwrap();
    assert!(settled.state.is_settled());
    assert_eq!(service.reads(), 3);
}

#[tokio::test]
async fn wait_returns_the_first_failed_read() {
    let service = Recording::reading(Value::Null, 10, Some(2));
    let client = ExecutionClient::new(service.clone()).with_poll_interval(Duration::from_millis(1));
    let error = client
        .wait(&ctx(), RunId::from(uuid::Uuid::new_v4()))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unavailable);
    assert_eq!(service.reads(), 2);
}

#[tokio::test]
async fn control_commands_reach_the_service_unchanged() {
    let service = Recording::new(Value::Null);
    let client = ExecutionClient::new(service.clone());
    let (ctx, id) = (ctx(), RunId::from(uuid::Uuid::new_v4()));
    assert_eq!(
        client.abort(&ctx, id, AbortOptions::new(0)).await.unwrap(),
        CancelResult::Requested
    );
    assert_eq!(
        client
            .cancel(&ctx, id, CancelOptions::new(0))
            .await
            .unwrap(),
        CancelResult::Unchanged
    );
    assert_eq!(
        client
            .retry(&ctx, id, ContinueOptions::new(0))
            .await
            .unwrap()
            .id,
        id
    );
    assert_eq!(
        client
            .resume(&ctx, id, ContinueOptions::new(0))
            .await
            .unwrap()
            .id,
        id
    );
    assert!(client.events(&ctx, id, 0, 10).await.unwrap().is_empty());
    assert_eq!(
        client.history(&ctx, id).await.unwrap_err().kind(),
        ErrorKind::Internal
    );
    assert_eq!(
        *service.controls.lock().unwrap(),
        ["abort", "cancel", "retry", "resume", "events"]
    );
}
