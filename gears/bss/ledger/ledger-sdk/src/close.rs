//! Period-close request/response value types for the in-process data-access
//! API. Infrastructure-free: a minimal `OPEN→CLOSED` transition gated by a
//! synchronous pre-close tie-out (REST close + reopen/dual-control arrive in a
//! later slice).

/// The outcome of a period-close call. `already_closed` is `true` when the
/// period was already `CLOSED` (the call is idempotent — a re-close is a no-op).
#[derive(Clone, Debug)]
pub struct CloseOutcome {
    pub period_id: String,
    pub already_closed: bool,
}
