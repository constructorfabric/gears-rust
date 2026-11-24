//! Domain layer of the quota-enforcement gear.

pub mod admission;
pub mod attribution;
pub mod bootstrap;
pub mod catalog;
pub mod engines;
pub mod error;
pub mod notifications;
pub mod operations;
pub mod pep;
pub mod plugins;
pub mod policies;
pub mod ports;
pub mod quotas;
pub mod readiness;
pub mod service;
pub mod tokens;
pub(crate) mod trusted;

pub use admission::{Admission, AdmissionTarget, Admitted};
pub use attribution::{AdmittedEvaluation, AdmittedReversal, Attribution, MappedAttribution};
pub use bootstrap::{Bootstrap, BootstrapReporting, Bound};
pub use catalog::MetricCatalog;
pub use error::{Dependency, DomainError, PluginKind, ResourceKind};
pub use plugins::PluginBinding;
pub use ports::{CoordinatorBinding, LeaderWork, SingletonCoordinator, SingletonScope};
pub use quotas::{GaugeTiming, LifecycleGaugeRefresher, QuotaLimits, QuotaManagement};
pub use readiness::{Readiness, ReadinessState};
pub use service::Service;
