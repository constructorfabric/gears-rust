//! Builds the real `IngestService`/`DeliveryService` pair over a shared
//! `Storage` - the single production wiring path (`module.rs::
//! register_rest()`), also used by `test_support::harness` so tests exercise
//! the same construction, not a second independently-maintained one
//! (`InMemoryDomainRepo`, which this supersedes - eb-single-process-
//! implementation design.md D2 risk mitigation).

use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;

use crate::api::rest::state::HandlerState;
use crate::domain::backend::BackendResolver;
use crate::domain::delivery::{DeliveryService, DeliveryServiceImpl};
use crate::domain::ingest::{IngestService, IngestServiceImpl};
use crate::domain::specification::SpecificationManager;
use crate::infra::storage::Storage;

/// `heartbeat_interval`: `None` keeps `DeliveryServiceImpl`'s own default
/// (test callers); production (`module.rs`) always passes `Some`, wired
/// from `StreamingConfig`.
#[must_use]
pub fn build_handler_state(
    storage: Arc<Storage>,
    policy_enforcer: PolicyEnforcer,
    spec_manager: Arc<dyn SpecificationManager>,
    backend_resolver: Arc<dyn BackendResolver>,
    heartbeat_interval: Option<Duration>,
) -> HandlerState {
    let ingest: Arc<dyn IngestService> = Arc::new(IngestServiceImpl::new(
        Arc::clone(&storage),
        policy_enforcer.clone(),
        Arc::clone(&spec_manager),
        Arc::clone(&backend_resolver),
    ));
    let mut delivery_impl =
        DeliveryServiceImpl::new(storage, policy_enforcer, spec_manager, backend_resolver);
    if let Some(interval) = heartbeat_interval {
        delivery_impl = delivery_impl.with_heartbeat_interval(interval);
    }
    let delivery: Arc<dyn DeliveryService> = Arc::new(delivery_impl);
    HandlerState { ingest, delivery }
}
