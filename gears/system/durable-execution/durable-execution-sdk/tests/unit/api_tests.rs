use super::*;
use crate::observation::{RecordedTime, RunState};
use crate::{EventKind, ExecutionOwner};
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
fn unused() -> CanonicalError {
    CanonicalError::internal("not used by this test").create()
}
#[async_trait]
impl DurableExecution for Scripted {
    async fn start(
        &self,
        _: &SecurityContext,
        _: &str,
        _: Value,
        _: StartOptions,
    ) -> Result<StartResult, CanonicalError> {
        Err(unused())
    }
    async fn get(&self, _: &SecurityContext, id: RunId) -> Result<RunProgress, CanonicalError> {
        let position = *self.position.lock().unwrap();
        Ok(self.progress(id, position))
    }
    async fn events(
        &self,
        _: &SecurityContext,
        id: RunId,
        after: u64,
        _: u32,
    ) -> Result<Vec<RunEvent>, CanonicalError> {
        if self.fail_events {
            return Err(CanonicalError::service_unavailable().create());
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
    async fn history(&self, _: &SecurityContext, _: RunId) -> Result<RunHistory, CanonicalError> {
        Err(unused())
    }
    async fn result(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: &str,
        _: &str,
        _: ResultSelector,
    ) -> Result<Value, CanonicalError> {
        Err(unused())
    }
    async fn abort(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: AbortOptions,
    ) -> Result<CancelResult, CanonicalError> {
        Err(unused())
    }
    async fn cancel(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: CancelOptions,
    ) -> Result<CancelResult, CanonicalError> {
        Err(unused())
    }
    async fn retry(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: ContinueOptions,
    ) -> Result<RunProgress, CanonicalError> {
        Err(unused())
    }
    async fn resume(
        &self,
        _: &SecurityContext,
        _: RunId,
        _: ContinueOptions,
    ) -> Result<RunProgress, CanonicalError> {
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
    let client = DurableExecutionClient::new(Scripted::new(lifecycle(), false))
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
    let client = DurableExecutionClient::new(Scripted::new(lifecycle(), true));
    let ctx = ctx();
    let items: Vec<_> = client
        .watch(&ctx, RunId::from(uuid::Uuid::new_v4()))
        .collect()
        .await;
    assert_eq!(items.len(), 2);
    assert!(items[0].is_ok());
    assert!(items[1].is_err());
}
