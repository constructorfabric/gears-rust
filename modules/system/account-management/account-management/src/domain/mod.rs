//! Domain layer for the Account Management module.
//!
//! Houses the error taxonomy, metric catalog, `IdP` provisioner contract,
//! tenant domain model + repository trait, the `TenantService`
//! domain-service layer, the [`tenant_type`] compatibility-barrier
//! abstraction (with the production `GtsTenantTypeChecker` wired through
//! `infra::types_registry`), the platform-bootstrap saga, and the
//! hierarchy-integrity vocabulary ([`tenant::integrity`]) consumed by
//! the Rust-side classifier pipeline in
//! [`crate::infra::storage::integrity`].
//!
//! Audit-event emission is **not** carried in this module. The platform
//! audit-bus contract is not specified yet; lifecycle observability
//! lives on `tracing::info!(target = "am.events" / "am.bootstrap" /
//! "am.integrity")` log lines and on the metric catalog. When the
//! transport contract lands, an `audit`-shaped sub-module can be
//! reintroduced without touching the call sites that already log.

pub mod bootstrap;
pub mod conversion;
pub mod error;
pub mod gts_validation;
pub mod idp;
pub mod integrity_check;
pub mod metadata;
pub mod metrics;
pub mod ports;
pub mod tenant;
pub mod tenant_type;
pub mod user;
pub mod user_groups;
pub mod util;
