//! Infrastructure adapters: the cluster coordination adapter, the PDP
//! reachability probe, the types-registry reads of the metric catalogue, the
//! canonical-error lift, the gauge sample cells, and the metrics meter.

pub mod canonical_mapping;
pub mod charge_backlog;
pub mod cluster_coordination;
pub mod lease_backlog;
pub mod lifecycle_gauges;
pub mod metrics;
pub mod pdp_probe;
pub mod types_registry;

pub use cluster_coordination::{
    ClusterCoordination, ClusterCoordinationBinding, ElectionTiming, QuotaEnforcementProfile,
    SCOPE_PREFIX,
};
pub use lifecycle_gauges::{
    LifecycleGaugeCell, QUOTA_CAP_UNBOUNDED_TOTAL, QUOTA_CAP_ZERO_TOTAL,
    QUOTA_FOR_DIRECT_METRIC_TOTAL,
};
pub use metrics::{
    ADMITTED_METRIC_VIOLATIONS_TOTAL, CONTRACT_VALIDATION_FAILURES_TOTAL, DENIAL_TOTAL,
    QeMetricsMeter, build_default_adapter,
};
pub use pdp_probe::{PROBE_SUBJECT, PdpReachability};
pub use types_registry::PlatformTypeRegistry;
