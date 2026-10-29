//! The gear's processing worker: started with the gear, stopped with it.

use std::num::NonZeroUsize;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use tokio_util::sync::CancellationToken;
use toolkit::contracts::RunnableCapability;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{ConstructGear, Running, lock};
use crate::config::{CategoryCheckConfig, ConstructConfig};
use crate::domain::model_client::{ModelClient, ModelError, ModelRequest, ModelResponse};
use crate::domain::record_intake::RecordHandOff;
use crate::infra::pipeline::assemble;
use crate::infra::processing_metrics::WorkerLoad;
use crate::infra::record_queue::record_queue;
use crate::test_support::{
    AllowResolver, fake_graph_storage, inmem_db, received_chat_record, settings_service,
    unchecked_processing,
};

/// A model no test record reaches.
struct Unused;

#[async_trait]
impl ModelClient for Unused {
    async fn complete(
        &self,
        _ctx: &SecurityContext,
        _request: &ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        Err(ModelError::Unavailable("not used".to_owned()))
    }
}

/// A gear holding a processing worker, as `init` leaves it, and the hand-off
/// intake would use.
async fn gear_with_worker() -> (ConstructGear, Arc<dyn RecordHandOff>) {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        processing: unchecked_processing(),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();
    let pipeline = assemble(
        &cfg,
        Some(Arc::new(Unused)),
        fake_graph_storage(),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
    )
    .unwrap();
    let gear = ConstructGear::default();
    *lock(&gear.worker) = pipeline.worker;
    (gear, pipeline.hand_off)
}

#[tokio::test]
async fn start_runs_the_worker_and_stop_ends_it() {
    let (gear, hand_off) = gear_with_worker().await;

    gear.start(CancellationToken::new()).await.unwrap();
    assert!(lock(&gear.worker).is_none(), "taken by start");
    let load = lock(&gear.running)
        .as_ref()
        .expect("the worker runs")
        .load
        .clone();
    hand_off
        .hand_off(received_chat_record(Uuid::new_v4(), Uuid::new_v4()))
        .unwrap();
    let drained = async {
        while load.queued() + load.in_flight() > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), drained)
        .await
        .expect("the started worker takes the record and processes it");

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        gear.stop(CancellationToken::new()),
    )
    .await
    .expect("stop returns once the worker ends")
    .unwrap();
    assert!(lock(&gear.running).is_none());
}

#[tracing_test::traced_test]
#[tokio::test]
async fn stop_after_its_deadline_aborts_a_worker_that_does_not_end() {
    let gear = ConstructGear::default();
    // A worker that never notices it was told to stop.
    let handle = tokio::spawn(std::future::pending::<()>());
    let aborted = handle.abort_handle();
    *lock(&gear.running) = Some(Running {
        token: CancellationToken::new(),
        handle,
        load: WorkerLoad::new(Weak::new()),
    });
    let deadline = CancellationToken::new();
    deadline.cancel();

    tokio::time::timeout(std::time::Duration::from_secs(5), gear.stop(deadline))
        .await
        .expect("stop returns at the deadline, not when the worker ends")
        .unwrap();

    assert!(
        aborted.is_finished(),
        "the worker is aborted before stop returns"
    );
    assert!(logs_contain("did not stop by the deadline"));
}

#[tracing_test::traced_test]
#[tokio::test]
async fn stop_says_how_many_records_it_loses() {
    let gear = ConstructGear::default();
    let (hand_off, _queue) = record_queue(NonZeroUsize::new(4).unwrap());
    let hand_off = Arc::new(hand_off);
    hand_off
        .hand_off(received_chat_record(Uuid::new_v4(), Uuid::new_v4()))
        .unwrap();
    let load = WorkerLoad::new(Arc::downgrade(&hand_off));
    let running = load.start();
    *lock(&gear.running) = Some(Running {
        token: CancellationToken::new(),
        handle: tokio::spawn(async {}),
        load,
    });

    gear.stop(CancellationToken::new()).await.unwrap();
    drop(running);

    assert!(logs_contain("queued=1"));
    assert!(logs_contain("in_flight=1"));
    assert!(logs_contain("records are lost"));
}

#[tokio::test]
async fn without_processing_start_and_stop_do_nothing() {
    let gear = ConstructGear::default();

    gear.start(CancellationToken::new()).await.unwrap();
    assert!(lock(&gear.running).is_none());
    gear.stop(CancellationToken::new()).await.unwrap();
}

#[tokio::test]
async fn a_worker_that_panicked_makes_stop_fail() {
    let gear = ConstructGear::default();
    let handle = tokio::spawn(async { panic!("the worker broke") });
    while !handle.is_finished() {
        tokio::task::yield_now().await;
    }
    *lock(&gear.running) = Some(Running {
        token: CancellationToken::new(),
        handle,
        load: WorkerLoad::new(Weak::new()),
    });

    let stopped = gear.stop(CancellationToken::new()).await;

    // The host logs the error stop returns; stop does not log it again.
    let err = stopped.unwrap_err();
    assert!(
        format!("{err:#}").contains("ended with an error"),
        "{err:#}"
    );
}

#[tracing_test::traced_test]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_gives_up_on_an_aborted_worker_that_does_not_end_and_says_so() {
    // A worker blocked in a call that never yields, so the abort cannot
    // reach it; it ends well after the grace stop waits for.
    let blocked = std::time::Duration::from_secs(3);
    let gear = ConstructGear::default();
    let started_blocking = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = tokio::spawn({
        let started_blocking = Arc::clone(&started_blocking);
        async move {
            started_blocking.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(blocked);
        }
    });
    // An abort before the worker first runs would end it cleanly; wait until
    // it is blocked.
    while !started_blocking.load(std::sync::atomic::Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    *lock(&gear.running) = Some(Running {
        token: CancellationToken::new(),
        handle,
        load: WorkerLoad::new(Weak::new()),
    });
    let deadline = CancellationToken::new();
    deadline.cancel();
    let started = std::time::Instant::now();

    let stopped = gear.stop(deadline).await;

    let err = stopped.expect_err("a worker that did not end is no clean stop");
    assert!(
        format!("{err:#}").contains("did not end after it was aborted"),
        "{err:#}"
    );
    assert!(
        started.elapsed() < blocked,
        "stop returns after its grace, before the worker ends: {:?}",
        started.elapsed()
    );
    assert!(logs_contain("did not end after it was aborted"));
}
