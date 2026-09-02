//! Domain layer: business logic, service traits, and repository contracts.
//! No infra dependencies (`DESIGN.md` §1.3 Architecture Layers).

pub mod cluster;
pub mod delivery;
pub mod error;
pub mod event_type;
pub mod idempotency;
pub mod ingest;
pub mod model;
pub mod projection;
pub mod repo;
pub mod specification;

pub use cluster::EventBrokerCluster;
pub use delivery::DeliveryService;
pub use error::DomainError;
pub use ingest::IngestService;
pub use specification::SpecificationManager;
