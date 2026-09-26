//! Builds the real `IngestService`/`DeliveryService` pair over a shared
//! `Storage` - the single production wiring path (`module.rs::
//! register_rest()`), also used by `test_support::harness` so tests exercise
//! the same construction, not a second independently-maintained one
//! (`InMemoryDomainRepo`, which this supersedes - eb-single-process-
//! implementation design.md D2 risk mitigation).

use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;

use tokio_util::sync::CancellationToken;

use crate::api::rest::state::HandlerState;
use crate::config::{BatchConfig, LoaderConfig, StreamingConfig};
use crate::domain::backend::BackendResolver;
use crate::domain::consumer_group_coordinator::ConsumerGroupCoordinator;
use crate::domain::delivery::{DeliveryService, DeliveryServiceImpl};
use crate::domain::ingest::{IngestService, IngestServiceImpl};
use crate::domain::specification::SpecificationManager;
use crate::domain::streaming::lease::InProcessStreamLeases;
use crate::infra::loader::backend_source::BackendEventSource;
use crate::infra::loader::poll::PollPolicy;
use crate::infra::loader::scheduler::{DemandScheduler, SchedulerPolicy};
use crate::infra::loader::shard::ShardLoader;
use crate::infra::loader::topics::{TopicManager, TopicPolicy};
use crate::infra::partition_cache::demand::StarvationWeight;
use crate::infra::partition_cache::reclaim::{
    GapThresholdEvents, ReclaimPolicy, ResidencyLimitBytes,
};
use crate::infra::storage::Storage;

/// Takes the whole `StreamingConfig` (batch bounds, progress cadence) plus the
/// heartbeat as a separate `Duration`.
///
/// The heartbeat is a `Duration` argument rather than read off
/// `StreamingConfig.heartbeat_interval_secs` so a test harness can run a
/// sub-second cadence without giving `StreamingConfig` a millisecond field;
/// production passes `Duration::from_secs(...)` of that same whole-second knob,
/// so its behaviour is unchanged.
// Each argument is a distinctly-typed, fully-required dependency and this is the
// single wiring path (called once from `module.rs`), so a builder or deps-struct
// would add indirection without preventing any real mistake - the same call as
// `DeliveryServiceImpl::new`, which carries the identical justification.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn build_handler_state(
    storage: Arc<Storage>,
    policy_enforcer: PolicyEnforcer,
    spec_manager: Arc<dyn SpecificationManager>,
    backend_resolver: Arc<dyn BackendResolver>,
    attacher: Arc<dyn crate::domain::streaming::source::ReaderAttacher>,
    groups: Arc<ConsumerGroupCoordinator>,
    leases: Arc<InProcessStreamLeases>,
    batch: BatchConfig,
    streaming: StreamingConfig,
    heartbeat_interval: std::time::Duration,
) -> HandlerState {
    let ingest: Arc<dyn IngestService> = Arc::new(IngestServiceImpl::new(
        Arc::clone(&storage),
        policy_enforcer.clone(),
        Arc::clone(&spec_manager),
        Arc::clone(&backend_resolver),
        batch,
    ));
    let delivery_impl = DeliveryServiceImpl::new(
        storage,
        policy_enforcer,
        spec_manager,
        backend_resolver,
        groups,
        attacher,
        leases,
        streaming,
        heartbeat_interval,
    );
    let delivery: Arc<dyn DeliveryService> = Arc::new(delivery_impl);
    HandlerState { ingest, delivery }
}

/// The partition caches this instance serves from, built from configuration.
///
/// Created in `init` rather than here-and-there because two callers need the
/// same one: `register_rest` hands it to the delivery service so a session can
/// attach readers, and `serve` hands it to the loader so those readers get
/// filled. Two managers would mean two sets of caches for one partition, with
/// readers on each believing they had its state.
#[must_use]
pub fn build_topic_manager(cfg: &LoaderConfig) -> Arc<TopicManager> {
    let reclaim = ReclaimPolicy::new(
        GapThresholdEvents(cfg.gap_threshold_events),
        ResidencyLimitBytes(cfg.residency_limit_bytes),
    );
    let policy = TopicPolicy::builder(reclaim)
        .fetch_max_events(cfg.fetch_max_events)
        .poll(
            PollPolicy::from_floor(Duration::from_millis(cfg.poll_floor_ms))
                .up_to(Duration::from_millis(cfg.poll_ceiling_ms)),
        )
        .build();
    Arc::new(TopicManager::new(policy))
}

/// Builds the instance's loader - what fills the caches ahead of readers, and
/// what reclaims behind them - and returns its run future, unspawned.
///
/// Returning the future rather than a `JoinHandle` lets the caller decide who
/// owns the task: `serve` registers it with its `Supervisor` so it is
/// supervised alongside every other worker, and the test harness spawns it
/// directly. Takes the `CancellationToken` the caller owns, so shutdown cancels
/// and drains rather than dropping a fetch in flight against a closing pool.
pub fn loader_future(
    cfg: &LoaderConfig,
    topics: Arc<TopicManager>,
    spec_manager: Arc<dyn SpecificationManager>,
    backend_resolver: Arc<dyn BackendResolver>,
    cancel: CancellationToken,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let source = Arc::new(BackendEventSource::new(spec_manager, backend_resolver));
    let scheduler = Arc::new(DemandScheduler::new(
        source,
        Arc::clone(&topics),
        SchedulerPolicy::with_pool(cfg.pool_permits)
            .starvation_weight(StarvationWeight(cfg.starvation_weight))
            .build(),
    ));
    ShardLoader::new(scheduler, topics, Duration::from_millis(cfg.tick_ms)).run(cancel)
}
