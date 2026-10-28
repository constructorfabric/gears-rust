// Internal capability and scoped adapters are consumed by the engine, draft service, reads and
// workers (S2-04/09/11, S6-01). Business operations remain unavailable until they are delivered.
/// The managed producer binding and its readiness (S2-08).
pub mod broker;
/// Draft authoring (S2-09): the service REST and the local SDK share.
pub mod capture;
/// Date policy channel, startup default check and admission-time date preparation (S2-10).
/// Preview, submit (S3-12) and amendment (S4) consume the preparer and admitted-line builder.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the date preparer and admitted-line builder have no caller until Preview, \
                  submit (S3-12) and amendment (S4) are delivered"
    )
)]
#[cfg_attr(
    test,
    allow(
        dead_code,
        reason = "the unit tests reach only part of the undelivered ports, so whether the lint \
                  fires differs per test target; `expect` cannot be verified there"
    )
)]
pub mod dates;
/// Log-safe decode diagnostics shared by the boundary and the storage mapping.
pub mod diagnostics;
/// The transition engine's transaction adapter (S2-04): the single writer of the aggregate,
/// versions, audit, registry settlement and transition events.
pub mod engine;
/// Typed events, their GTS contract and the transition-transaction enqueue (S2-08). The S2-04
/// engine is the production caller; no business operation emits yet.
pub mod events;
pub mod execution;
pub mod maintenance;
/// Authorized reads with access logging (early S6-01/S6-04).
pub mod read;
pub mod storage;
/// The five Orders-owned workers: scheduling, locking, class connections, retention purge,
/// idempotency cleanup with the D-188/D-198 continuation port, and audit verification/
/// checkpointing (S2-11). Expiry/auto-void bodies are Stage 5 ports.
pub mod workers;
