//! The shared gate assessment's pure core (03 §3.1–§3.4; D-187, D-195, D-231): what the
//! assessment decides over typed port answers. Resolution (`infra::gate`) and the engine
//! (`infra::engine`) are outside; this module makes no call and touches no storage.

/// The per-port circuit breaker state machine (03 §3.3, D-237).
pub mod breaker;

/// The outbound port inventory and typed unavailability (D-231 items 1 and 2).
pub mod ports;

/// Orders mapping/coverage registry version persisted with every outcome (D-195, `mapping_version`).
pub const MAPPING_VERSION: &str = "orders-gate-mapping/1";

/// SDK-shaped port request and answer types (Rating #5390/#5175, Subscriptions #5131; D-189/D-191/D-193/D-199).
pub mod answers;
/// The pure assessment over typed port answers (S3-09): vector, failure list, engine inputs, pins.
pub mod assess;
/// Submit's frozen per-line Pricing requests (S3-12, D-188, D-192).
pub mod commercial;
/// D-195 composition of producer subresults into one outcome.
pub mod compose;
/// Allowlisted upstream evidence carried on outcomes.
pub mod evidence;
/// Proposed complete item classification evidence and collection validation.
pub mod item_keys;
/// The orchestrator's pure half: per-port queries from the basket and assembly of the answers.
pub mod orchestrate;
/// The declared predicate registry (F03 §3.4 predicate order).
pub mod predicates;
/// D-187 typed Pricing/Products read mapping into adopted predicate outcomes and the pin.
pub mod pricing_map;
/// The activation re-check's pure decision Workflow executes (03 §3.6, D-127, D-144).
pub mod recheck;
/// The pure Preview result shaping (S3-11, D-125): assessment and answers into `PreviewResult`.
pub mod report;
/// What one assessment contributes to the run-level signals (03 §3.8, D-237).
pub mod signals;
/// Submit's `resolved_total` rows from the frozen evaluation (S3-12, D-197, D-216).
pub mod totals;
/// The ordered tri-state outcome vector.
pub mod vector;

/// Strict proposed protected occupancy observation and shared validation.
pub mod protected;
