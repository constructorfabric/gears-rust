//! Pure Orders domain rules. The application services that enter the engine live in `infra`.

// Consumed by the S2-04 engine and its slices.
#[allow(dead_code)]
pub mod audit;
/// Draft capture (S2-09): trigger selection, capture guards and authored term geometry.
pub mod capture;
/// Date cascade and policy provenance (S2-10). Preview, submit (S3-12) and amendment (S4) bind
/// its basis; none of them is delivered yet.
#[allow(dead_code)]
pub mod dates;
#[allow(dead_code)]
pub mod idempotency;
#[allow(dead_code)]
pub mod overlap;
/// Read surfaces (early S6-01/S6-04): page bounds, cursor codec and the access-log decision.
pub mod read;
// The S2-04 engine's declarative core. Slice operations bind guards, select triggers from the
// field classes and supply contributions.
#[allow(dead_code)]
pub mod contributions;
#[allow(dead_code)]
pub mod guards;
#[allow(dead_code)]
pub mod state_table;
#[allow(dead_code)]
pub mod transition;
