//! Dual-control approval orchestration (VHP-1852): the [`service::ApprovalService`]
//! lifecycle engine and the [`service::ApprovalExecutor`] port it dispatches the
//! governed mutation through on approve. Lives in `infra` (needs repo +
//! transaction access); the pure state types + threshold policy stay in
//! `crate::domain::approval`.

pub mod executor;
pub mod service;
