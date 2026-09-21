//! Domain layer of the quota-enforcement gear.

pub mod error;
pub mod ports;

pub use error::{Dependency, DomainError, PluginKind, ResourceKind};
pub use ports::{CoordinatorBinding, LeaderWork, SingletonCoordinator, SingletonScope};
