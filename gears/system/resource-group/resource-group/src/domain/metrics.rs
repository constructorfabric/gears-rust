// Created: 2026-08-07 by Constructor Tech
//! Metric port for the write paths, and the vocabulary its attributes use.
//!
//! Declared here and implemented in `infra::metrics` so the domain names what
//! it wants measured without depending on OpenTelemetry.
//!
//! ## What this covers, and what it does not
//!
//! The concurrent-write analysis asks for a measurement layer before any
//! further tuning, and separates two questions that raw query throughput
//! conflates: how much work commits, and how much work is done and thrown
//! away. This port answers the first from where the gear can see it -- an
//! operation's wall time, the size of the subtree it touched, the closure
//! rows it wrote.
//!
//! The second needs the retry loop's own counters (attempts, serialization
//! failures, exhausted budgets), and that loop lives in `toolkit-db`, which
//! carries no instruments at all today. Adding them there is a platform
//! change touching every gear and belongs in its own review, not smuggled
//! into a resource-group PR.

/// The write operation a measurement belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Create,
    Update,
    Move,
    Delete,
    ForceDelete,
}

impl Operation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create_group",
            Self::Update => "update_group",
            Self::Move => "move_group",
            Self::Delete => "delete_group",
            Self::ForceDelete => "force_delete_group",
        }
    }
}

/// Whether the operation returned a value or an error.
///
/// Deliberately two-valued. A per-error-kind breakdown belongs to the error
/// taxonomy, not here; this exists so a duration histogram can separate the
/// time spent on work that succeeded from time spent on work that did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Error,
}

impl Outcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

/// Records what the write paths measure.
///
/// Every method takes `&self` and must not fail: a metric that can refuse
/// would put a second failure mode on a path that already has one.
pub trait RgMetricsPort: Send + Sync {
    /// Wall time of one operation, from the caller's entry to its return --
    /// including the retries the caller cannot see individually.
    fn operation_duration(&self, operation: Operation, outcome: Outcome, seconds: f64);

    /// Nodes in the subtree an operation touched. Recorded for the two
    /// operations whose cost is a function of it, so a slow tail can be read
    /// against the shape of the input rather than guessed at.
    fn subtree_nodes(&self, operation: Operation, nodes: u64);

    /// Closure rows an operation wrote. With the rebuild set-based, this is
    /// the database's own count -- the rows are never materialized in the
    /// process, so nothing else here knows the number.
    fn closure_rows_written(&self, operation: Operation, rows: u64);

    /// An `update_group` that opened below SERIALIZABLE and then found it
    /// needed the move branch, so the whole operation ran again.
    ///
    /// The isolation choice is made from a pre-transaction hint; this counts
    /// how often that hint was wrong in the direction that costs a rerun. A
    /// rate that stops being negligible is the signal to reconsider the hint,
    /// not to loosen the guard.
    fn isolation_escalation(&self, operation: Operation);

    /// Time spent resolving and compiling the GTS metadata schema.
    ///
    /// Measured because it moved out of the transaction: it is a network
    /// round-trip plus a schema compile, and knowing its cost is what makes
    /// the case for caching the compiled validator concrete instead of
    /// assumed.
    fn metadata_validation_duration(&self, operation: Operation, seconds: f64);
}

/// A port implementation that records nothing.
///
/// For call sites constructed without a meter -- the global provider is
/// already a no-op until an exporter is wired, so this exists for tests and
/// for constructing a service without reaching for the global at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopMetrics;

impl RgMetricsPort for NoopMetrics {
    fn operation_duration(&self, _operation: Operation, _outcome: Outcome, _seconds: f64) {}
    fn subtree_nodes(&self, _operation: Operation, _nodes: u64) {}
    fn closure_rows_written(&self, _operation: Operation, _rows: u64) {}
    fn isolation_escalation(&self, _operation: Operation) {}
    fn metadata_validation_duration(&self, _operation: Operation, _seconds: f64) {}
}
