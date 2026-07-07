//! Outbound port traits — interfaces the domain calls. Adapters live under
//! `infra/` (e.g. `crate::infra::metrics` implements [`metrics::LedgerMetricsPort`]).

pub mod metrics;
pub mod obligation_state;
