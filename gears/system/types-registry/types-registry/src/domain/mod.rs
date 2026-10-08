//! Domain layer: database and legacy in-memory paths coexist until T26–T30.
//!
//! | Database | Legacy | Responsibility |
//! |---|---|---|
//! | [`ports`] | [`repo`] | Persistence |
//! | [`registry_service`] | [`service`] | Domain API |
//! | [`enums`] + [`ports`] rows | [`model`] | Domain vocabulary |
//!
//! Group modules by concept. [`admission`] has a directory for its six-module pipeline.

// ---------------------------------------------------------------------------
// The database-backed path (P0)
// ---------------------------------------------------------------------------

// The synchronous acceptance path and the request identity it rests on (T7).
pub mod admission;
// Materialized effective artifacts and the resolution fingerprint (SPEC D3).
pub mod artifacts;
// Compatibility against one baseline: which definition, and the verdict (ADR-0003).
pub mod compat;
// The three direct dependency edge kinds, extracted from authored content and the identifier.
pub mod dependency;
// Version-family key derivation and the three family rules.
pub mod family;
// How a caller names one entity: GTS identifier or Registry Reference.
pub mod key;
// The transient `gts-rust` store, one per admission unit (SPEC D2, §8.2).
pub mod gts_store;
// The registration-policy allowlist (DESIGN §3.2, SPEC §10.3).
pub mod policy;
// The persistence ports, and the rows and inputs that cross them.
pub mod ports;
// The database-backed domain surface every transport adapter calls (SPEC §8.4).
pub mod registry_service;
// Whether redelivering an admission can reach a different answer (T21).
pub mod retry;
// The normalized field set all three reads project by (T22b, SPEC §10.2).
pub mod selection;
// Freshness validators for conditional exact reads (T22d, SPEC §8.5).
pub mod validator;

// ---------------------------------------------------------------------------
// Shared by both paths
// ---------------------------------------------------------------------------

// The domain's own enumeration vocabularies, free of the storage numbering.
pub mod enums;
pub mod error;

// Legacy: repo, service and model retire with the old client and its cache at T31.
// New callers use ports and registry_service.

pub mod model;
pub mod repo;
pub mod service;

// === LOCAL CLIENT ===
// Survives the cutover but is retyped onto `EntitySnapshot` at T30, when the old
// models go.
pub mod local_client;

pub use error::DomainError;
pub use repo::GtsRepository;
pub use service::TypesRegistryService;
