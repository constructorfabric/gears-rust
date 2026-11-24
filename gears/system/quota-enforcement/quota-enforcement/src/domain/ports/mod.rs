//! Output ports the domain depends on.

pub mod coordination;
pub mod lifecycle_gauges;
pub mod metrics;
pub mod pdp;
pub mod type_registry;

pub use coordination::{
    CoordinatorBinding, LeaderWork, LeaderWorkFuture, SingletonCoordinator, SingletonScope,
};
pub use lifecycle_gauges::{LifecycleCounts, LifecycleGaugeSink, NoopGaugeSink};
pub use metrics::{
    ChargeBacklogSink, DenialReason, LeaseBacklogSink, MetricLabel, NoopChargeBacklog,
    NoopLeaseBacklog, NoopMetrics, QeMetrics, ValidationReason, ValidationSurface,
};
pub use pdp::PdpProbe;
pub use type_registry::{RegisteredType, TypeRegistry};
