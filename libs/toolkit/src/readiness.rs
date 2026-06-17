//! Process-level readiness state for the eventual-readiness `/readyz` probe.
//!
//! Orthogonal to the per-gear [`lifecycle::Status`](crate::lifecycle::Status)
//! (which tracks one gear's task run-state). `ReadinessState` is a single
//! process-wide signal an orchestrator (k8s readiness probe) consults to decide
//! when the pod is safe to receive traffic.
//!
//! Model (ADR-0007, v1 subset):
//! - **Starting** — at least one consumed dependency has not yet resolved in the
//!   directory. `/readyz` → 503.
//! - **Ready** — every consumed dependency resolved (or there are none).
//!   `/readyz` → 200.
//! - **Draining** — shutdown has begun; set on the root cancellation. `/readyz`
//!   → 503 so the orchestrator drains the pod out of the load balancer.
//! - **Degraded** — reserved for future custom readiness checks; never set in
//!   v1.
//!
//! Readiness is **startup-gating and sticky**: once a dependency is resolved it
//! stays resolved. A provider vanishing later does NOT revert `/readyz` — that
//! is runtime churn, handled lazily by the directory-resolving client at call
//! time (liveness is `/healthz`, not `/readyz`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::RwLock;

/// Coarse process readiness phase reported by `/readyz`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessPhase {
    /// Consumed dependencies still resolving.
    Starting,
    /// All consumed dependencies resolved (or none declared).
    Ready,
    /// Reserved for custom readiness checks (unused in v1).
    Degraded,
    /// Shutdown in progress.
    Draining,
}

impl ReadinessPhase {
    /// Stable lowercase wire string (`"starting"`, `"ready"`, …).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ReadinessPhase::Starting => "starting",
            ReadinessPhase::Ready => "ready",
            ReadinessPhase::Degraded => "degraded",
            ReadinessPhase::Draining => "draining",
        }
    }
}

/// A point-in-time readiness snapshot.
#[derive(Debug, Clone)]
pub struct ReadinessReport {
    /// Current phase.
    pub phase: ReadinessPhase,
    /// Names of consumed dependency gears not yet resolved.
    pub unresolved_deps: Vec<String>,
}

impl ReadinessReport {
    /// HTTP status code for a `/readyz` probe: 200 when serving traffic
    /// (`Ready`/`Degraded`), 503 otherwise (`Starting`/`Draining`).
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self.phase {
            ReadinessPhase::Ready | ReadinessPhase::Degraded => 200,
            ReadinessPhase::Starting | ReadinessPhase::Draining => 503,
        }
    }

    /// JSON body for a `/readyz` probe.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.phase.as_str(),
            "unresolved_deps": self.unresolved_deps,
        })
    }
}

/// Process-level readiness, shared via `Arc` and published in the `ClientHub`
/// so the gateway's `/readyz` handler and the runtime's readiness-probe loop can
/// both reach it.
#[derive(Debug, Default)]
pub struct ReadinessState {
    draining: AtomicBool,
    /// `dep_gear` → resolved. Populated by the proxy-wiring phase from each
    /// `#[toolkit::consumes]` registration; flipped true once resolved.
    deps: RwLock<BTreeMap<String, bool>>,
}

impl ReadinessState {
    /// Create an empty readiness state (no deps → trivially `Ready`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a consumed dependency that gates readiness (idempotent; keeps an
    /// already-resolved entry resolved).
    pub fn register_dep(&self, dep_gear: impl Into<String>) {
        self.deps.write().entry(dep_gear.into()).or_insert(false);
    }

    /// Mark a previously-registered dependency as resolved.
    pub fn mark_resolved(&self, dep_gear: &str) {
        if let Some(resolved) = self.deps.write().get_mut(dep_gear) {
            *resolved = true;
        }
    }

    /// Begin draining (terminal): `/readyz` reports `Draining` regardless of deps.
    pub fn set_draining(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    /// Whether shutdown has begun.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Snapshot the current readiness.
    #[must_use]
    pub fn report(&self) -> ReadinessReport {
        if self.is_draining() {
            return ReadinessReport {
                phase: ReadinessPhase::Draining,
                unresolved_deps: Vec::new(),
            };
        }
        let unresolved: Vec<String> = self
            .deps
            .read()
            .iter()
            .filter(|(_, resolved)| !**resolved)
            .map(|(name, _)| name.clone())
            .collect();
        let phase = if unresolved.is_empty() {
            ReadinessPhase::Ready
        } else {
            ReadinessPhase::Starting
        };
        ReadinessReport {
            phase,
            unresolved_deps: unresolved,
        }
    }

    /// Whether the process is serving traffic (`Ready` or `Degraded`).
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(
            self.report().phase,
            ReadinessPhase::Ready | ReadinessPhase::Degraded
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn no_deps_is_ready() {
        let s = ReadinessState::new();
        let r = s.report();
        assert_eq!(r.phase, ReadinessPhase::Ready);
        assert!(r.unresolved_deps.is_empty());
        assert_eq!(r.http_status(), 200);
        assert!(s.is_ready());
    }

    #[test]
    fn unresolved_dep_is_starting_with_list() {
        let s = ReadinessState::new();
        s.register_dep("billing");
        s.register_dep("inventory");
        let r = s.report();
        assert_eq!(r.phase, ReadinessPhase::Starting);
        assert_eq!(r.unresolved_deps, vec!["billing".to_owned(), "inventory".to_owned()]);
        assert_eq!(r.http_status(), 503);
        assert!(!s.is_ready());
    }

    #[test]
    fn ready_once_all_deps_resolved() {
        let s = ReadinessState::new();
        s.register_dep("billing");
        s.register_dep("inventory");
        s.mark_resolved("billing");
        assert_eq!(s.report().phase, ReadinessPhase::Starting);
        s.mark_resolved("inventory");
        let r = s.report();
        assert_eq!(r.phase, ReadinessPhase::Ready);
        assert_eq!(r.http_status(), 200);
    }

    #[test]
    fn register_dep_is_idempotent_and_preserves_resolved() {
        let s = ReadinessState::new();
        s.register_dep("billing");
        s.mark_resolved("billing");
        s.register_dep("billing"); // must NOT reset to unresolved
        assert_eq!(s.report().phase, ReadinessPhase::Ready);
    }

    #[test]
    fn draining_overrides_ready() {
        let s = ReadinessState::new();
        assert_eq!(s.report().phase, ReadinessPhase::Ready);
        s.set_draining();
        let r = s.report();
        assert_eq!(r.phase, ReadinessPhase::Draining);
        assert_eq!(r.http_status(), 503);
        assert!(!s.is_ready());
    }

    #[test]
    fn report_json_shape() {
        let s = ReadinessState::new();
        s.register_dep("billing");
        let j = s.report().to_json();
        assert_eq!(j["state"], "starting");
        assert_eq!(j["unresolved_deps"][0], "billing");
    }

    #[test]
    fn mark_resolved_unknown_dep_is_noop() {
        let s = ReadinessState::new();
        s.mark_resolved("nope"); // no panic, no entry created
        assert_eq!(s.report().phase, ReadinessPhase::Ready);
    }
}
