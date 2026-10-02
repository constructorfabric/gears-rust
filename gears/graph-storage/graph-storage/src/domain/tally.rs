//! How a batch landed, counted once for both store implementations.
//!
//! The counters are what every ingest answers with; the per-item outcomes are
//! what `options.report_per_item` opts into. Keeping the two in one place is
//! what stops them disagreeing: a store records each item exactly once, and
//! the counter and the outcome list are two views of that one record.

use graph_storage_sdk::models::{IngestCounts, ItemOutcome};

/// The counters and, when kept, the two per-item lists in batch order.
pub type TallyParts = (
    IngestCounts,
    Option<Vec<ItemOutcome>>,
    Option<Vec<ItemOutcome>>,
);

/// The batch's counters, plus the per-item outcomes when the caller asked.
#[derive(Debug, Default)]
pub struct IngestTally {
    pub counts: IngestCounts,
    nodes: Option<Vec<ItemOutcome>>,
    edges: Option<Vec<ItemOutcome>>,
}

impl IngestTally {
    /// A tally that keeps per-item outcomes only when `report_per_item` is set;
    /// otherwise the batch costs nothing beyond its counters.
    #[must_use]
    pub fn new(report_per_item: bool) -> Self {
        Self {
            counts: IngestCounts::default(),
            nodes: report_per_item.then(Vec::new),
            edges: report_per_item.then(Vec::new),
        }
    }

    /// Record how the next node of the batch landed. Returns whether stored
    /// state changed — everything but `Unchanged` does.
    pub fn node(&mut self, outcome: &ItemOutcome) -> bool {
        match outcome {
            ItemOutcome::Inserted => self.counts.nodes_inserted += 1,
            ItemOutcome::Updated => self.counts.nodes_updated += 1,
            ItemOutcome::Unchanged => self.counts.nodes_unchanged += 1,
            ItemOutcome::Materialized => self.counts.phantoms_materialized += 1,
        }
        if let Some(nodes) = &mut self.nodes {
            nodes.push(outcome.clone());
        }
        *outcome != ItemOutcome::Unchanged
    }

    /// Record how the next edge of the batch landed. An edge is never
    /// materialized, so that outcome counts as an update.
    pub fn edge(&mut self, outcome: &ItemOutcome) -> bool {
        match outcome {
            ItemOutcome::Inserted => self.counts.edges_inserted += 1,
            ItemOutcome::Updated | ItemOutcome::Materialized => self.counts.edges_updated += 1,
            ItemOutcome::Unchanged => self.counts.edges_unchanged += 1,
        }
        if let Some(edges) = &mut self.edges {
            edges.push(outcome.clone());
        }
        *outcome != ItemOutcome::Unchanged
    }

    /// A phantom endpoint was created for an edge that named an unknown key.
    /// Not a batch item, so it has no per-item slot.
    pub fn phantom_created(&mut self) {
        self.counts.phantoms_created += 1;
    }

    /// The counters and, when kept, the two outcome lists in batch order.
    #[must_use]
    pub fn into_parts(self) -> TallyParts {
        (self.counts, self.nodes, self.edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_are_kept_in_batch_order_only_when_asked() {
        let mut silent = IngestTally::new(false);
        assert!(silent.node(&ItemOutcome::Inserted));
        assert!(!silent.node(&ItemOutcome::Unchanged));
        let (counts, nodes, edges) = silent.into_parts();
        assert_eq!((counts.nodes_inserted, counts.nodes_unchanged), (1, 1));
        assert!(nodes.is_none() && edges.is_none());

        let mut told = IngestTally::new(true);
        told.node(&ItemOutcome::Materialized);
        told.node(&ItemOutcome::Updated);
        told.edge(&ItemOutcome::Unchanged);
        told.phantom_created();
        let (counts, nodes, edges) = told.into_parts();
        assert_eq!(counts.phantoms_materialized, 1);
        assert_eq!(counts.nodes_updated, 1);
        assert_eq!(counts.edges_unchanged, 1);
        assert_eq!(counts.phantoms_created, 1);
        assert_eq!(
            nodes.as_deref(),
            Some(&[ItemOutcome::Materialized, ItemOutcome::Updated][..])
        );
        assert_eq!(edges.as_deref(), Some(&[ItemOutcome::Unchanged][..]));
    }
}
