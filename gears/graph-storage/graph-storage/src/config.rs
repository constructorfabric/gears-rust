//! Typed configuration for the graph-storage gear.
//!
//! Every bound below is one row of DESIGN § Capacity and Admission Contract:
//! a default plus a hard range, and a value outside the hard range is
//! rejected at startup rather than clamped — a deployment that asks for the
//! impossible should not boot into something else silently.

use serde::Deserialize;

/// Which backend serves one-hop expansion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HopStrategy {
    /// One-statement SQL/PGQ `GRAPH_TABLE` hop (requires `PostgreSQL` 19+).
    #[default]
    Pgq,
    /// Two scoped queries; the universal fallback every deployment can serve.
    TwoQuery,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphStorageConfig {
    /// Traversal backend. SQL/PGQ falls back to `two_query` per request when
    /// the scope defeats it, always with a logged reason.
    pub traversal_hop: HopStrategy,

    /// Vector width of the deployment's single embedding space. Fixed at
    /// migration time; readiness verifies configured == column definition.
    pub embedding_dimension: u32,

    // --- limits (graph-storage.limits.*) ----------------------------------
    pub ingest_max_nodes: u32,
    pub ingest_max_edges: u32,
    pub payload_max_bytes: u32,
    pub item_max_bytes: u32,
    pub node_read_max_adjacency: u32,
    pub traversal_max_depth: u8,
    pub traversal_max_nodes: u32,
    pub traversal_max_frontier: u32,
    pub traversal_max_edges_scanned: u64,
    pub search_max_arm_limit: u32,
    pub projection_max_page: u32,
    /// Absolute deadline for interactive operations, seconds.
    pub deadline_interactive_secs: u64,
    /// Idempotency receipt retention, days.
    pub idempotency_retention_days: u32,
}

impl Default for GraphStorageConfig {
    fn default() -> Self {
        Self {
            traversal_hop: HopStrategy::default(),
            embedding_dimension: 384,
            ingest_max_nodes: 10_000,
            ingest_max_edges: 20_000,
            payload_max_bytes: 64 * 1024,
            item_max_bytes: 256 * 1024,
            node_read_max_adjacency: 100,
            traversal_max_depth: 5,
            traversal_max_nodes: 1_000,
            traversal_max_frontier: 10_000,
            traversal_max_edges_scanned: 100_000,
            search_max_arm_limit: 50,
            projection_max_page: 200,
            deadline_interactive_secs: 10,
            idempotency_retention_days: 7,
        }
    }
}

/// One hard range violated => one line naming the key, the value and the
/// permitted range, so the boot failure is actionable without reading code.
macro_rules! check_range {
    ($errors:ident, $cfg:ident, $field:ident, $min:expr, $max:expr) => {
        #[allow(unused_comparisons)]
        if $cfg.$field < $min || $cfg.$field > $max {
            $errors.push(format!(
                concat!(
                    "graph-storage.limits.",
                    stringify!($field),
                    " = {} is outside the hard range {}..={}"
                ),
                $cfg.$field, $min, $max
            ));
        }
    };
}

impl GraphStorageConfig {
    /// Enforce the hard ranges of the Capacity and Admission Contract.
    pub fn validate(&self) -> anyhow::Result<()> {
        let mut errors: Vec<String> = Vec::new();
        check_range!(errors, self, embedding_dimension, 1u32, 4_096u32);
        check_range!(errors, self, ingest_max_nodes, 1u32, 50_000u32);
        check_range!(errors, self, ingest_max_edges, 1u32, 100_000u32);
        check_range!(errors, self, payload_max_bytes, 1_024u32, 1_048_576u32);
        check_range!(errors, self, item_max_bytes, 4_096u32, 4_194_304u32);
        check_range!(errors, self, node_read_max_adjacency, 1u32, 1_000u32);
        check_range!(errors, self, traversal_max_depth, 1u8, 8u8);
        check_range!(errors, self, traversal_max_nodes, 1u32, 10_000u32);
        check_range!(errors, self, traversal_max_frontier, 1u32, 100_000u32);
        check_range!(
            errors,
            self,
            traversal_max_edges_scanned,
            1u64,
            10_000_000u64
        );
        check_range!(errors, self, search_max_arm_limit, 1u32, 500u32);
        check_range!(errors, self, projection_max_page, 1u32, 1_000u32);
        check_range!(errors, self, deadline_interactive_secs, 1u64, 300u64);
        check_range!(errors, self, idempotency_retention_days, 1u32, 365u32);
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "invalid graph-storage configuration:\n  {}",
                errors.join("\n  ")
            )
        }
    }

    /// The interactive deadline as a duration.
    #[must_use]
    pub fn deadline_interactive(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.deadline_interactive_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pass_validation() {
        GraphStorageConfig::default()
            .validate()
            .unwrap_or_else(|e| panic!("defaults must validate: {e}"));
    }

    #[test]
    fn a_value_outside_the_hard_range_is_rejected_by_name() {
        let cfg = GraphStorageConfig {
            traversal_max_depth: 9,
            ..GraphStorageConfig::default()
        };
        let message = match cfg.validate() {
            Err(error) => error.to_string(),
            Ok(()) => panic!("depth 9 must be rejected"),
        };
        assert!(message.contains("traversal_max_depth"), "{message}");
        assert!(message.contains("1..=8"), "{message}");
    }
}
