//! Domain layer of the quota-enforcement gear.

pub mod admission;
pub mod error;
pub mod pep;
pub mod plugins;
pub mod ports;

pub use admission::{Admission, AdmissionTarget, Admitted};
pub use error::{Dependency, DomainError, PluginKind, ResourceKind};
pub use plugins::PluginBinding;
pub use ports::{CoordinatorBinding, LeaderWork, SingletonCoordinator, SingletonScope};
