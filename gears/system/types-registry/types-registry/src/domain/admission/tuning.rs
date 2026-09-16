//! The configuration one admission pass obeys, shared by the committing worker
//! and the dry-run prediction pass.
//!
//! It lives beside them rather than in either, because both passes take it and a
//! shared owner is what keeps the two modules from depending on each other.

use std::sync::Arc;

use crate::config::{Limits, WorkerSettings};
use crate::domain::ports::metrics::AdmissionMetrics;

/// The configuration one admission pass obeys, carried together.
#[derive(Clone, Copy)]
pub struct Tuning<'a> {
    pub limits: &'a Limits,
    pub worker: &'a WorkerSettings,
    pub metrics: &'a Arc<dyn AdmissionMetrics>,
    /// Deployment waiver setting for this pass, including retries and revalidation.
    /// See [`effective_force`].
    pub allow_compatibility_force: bool,
}

/// Clear a stored waiver when the deployment disables it. The candidate then
/// receives the ordinary verdict, and provenance records the cleared flag.
pub(super) const fn effective_force(item_forced: bool, tuning: &Tuning<'_>) -> bool {
    item_forced && tuning.allow_compatibility_force
}
