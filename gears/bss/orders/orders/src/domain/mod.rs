//! Pure Orders domain rules. The application services that enter the engine live in `infra`.

pub mod audit;
/// Draft capture (S2-09): trigger selection, capture guards and authored term geometry.
pub mod capture;
/// Date cascade and policy provenance (S2-10). Preview, submit (S3-12) and amendment (S4) bind
/// its basis; none of them is delivered yet.
pub mod dates;
/// The engine's caller-visible failure and its projection into the SDK error.
pub mod error;
/// The closed fulfillment failure reasons shared by the audit rule and the event contract.
pub mod failure;
pub mod idempotency;
pub mod overlap;
/// Read surfaces (early S6-01/S6-04): page bounds, cursor codec and the access-log decision.
pub mod read;
// The S2-04 engine's declarative core. Slice operations bind guards, select triggers from the
// field classes and supply contributions.
pub mod contributions;
pub mod guards;
pub mod state_table;
pub mod transition;
