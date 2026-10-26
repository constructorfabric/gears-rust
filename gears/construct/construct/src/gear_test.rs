//! The gear's processing worker: started with the gear, stopped with it.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use tokio_util::sync::CancellationToken;
use toolkit::contracts::RunnableCapability;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{ConstructGear, lock};
use crate::config::ConstructConfig;
use crate::domain::model_client::{ModelClient, ModelError, ModelRequest, ModelResponse};
use crate::infra::pipeline::assemble;
use crate::test_support::{AllowResolver, fake_graph_storage, inmem_db, settings_service};

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

/// A gear holding a processing worker, as `init` leaves it.
async fn gear_with_worker() -> ConstructGear {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        ..ConstructConfig::default()
    };
    let pipeline = assemble(
        &cfg,
        Some(Arc::new(Unused)),
        Some(fake_graph_storage()),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
    )
    .unwrap();
    let gear = ConstructGear::default();
    *lock(&gear.worker) = pipeline.worker;
    gear
}

#[tokio::test]
async fn start_runs_the_worker_and_stop_ends_it() {
    let gear = gear_with_worker().await;

    gear.start(CancellationToken::new()).await.unwrap();
    assert!(lock(&gear.running).is_some(), "the worker runs");
    assert!(lock(&gear.worker).is_none(), "taken by start");

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        gear.stop(CancellationToken::new()),
    )
    .await
    .expect("stop returns once the worker ends")
    .unwrap();
    assert!(lock(&gear.running).is_none());
}

#[tokio::test]
async fn stop_after_its_deadline_aborts_the_worker() {
    let gear = gear_with_worker().await;
    gear.start(CancellationToken::new()).await.unwrap();
    let deadline = CancellationToken::new();
    deadline.cancel();

    gear.stop(deadline).await.unwrap();

    assert!(lock(&gear.running).is_none());
}

#[tokio::test]
async fn without_processing_start_and_stop_do_nothing() {
    let gear = ConstructGear::default();

    gear.start(CancellationToken::new()).await.unwrap();
    assert!(lock(&gear.running).is_none());
    gear.stop(CancellationToken::new()).await.unwrap();
}
