//! Dependency writes and bounded stored reads corrected by overlay changes.
//! Graph walks live in [`super::walk`]; corrections touch only batch-modified entities.

use std::collections::HashSet;

use async_trait::async_trait;
use toolkit_db::DbTx;
use toolkit_db::secure::{AccessScope, ScopeError};

use super::{AdmissionView, unsupported};
use crate::domain::enums::DependencyKind;
use crate::domain::ports::{
    DependencyClosure, DependencyEdgeRow, DependencyStore, EdgeSide, EntityEdge, ReverseImpact,
};

#[async_trait]
impl DependencyStore for AdmissionView {
    /// Correct the stored answer using the overlay. Read `touched + 1` distinct IDs:
    /// at least one survives if the overlay cannot disqualify them all.
    async fn has_live_direct_instances(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        type_schema_entity_id: i64,
    ) -> Result<bool, ScopeError> {
        let graph = self.graph().await;
        if type_schema_entity_id > 0 {
            let limit = graph.touched().len().saturating_add(1);
            let stored = self
                .base
                .live_direct_dependent_ids(
                    tx,
                    scope,
                    type_schema_entity_id,
                    Some(DependencyKind::InstanceOf),
                    limit,
                )
                .await?;
            if stored.iter().any(|id| !graph.touched().contains(id)) {
                return Ok(true);
            }
        }
        Ok(graph.sources().any(|(from, edges)| {
            graph.is_live(from)
                && edges.iter().any(|(kind, to)| {
                    *kind == DependencyKind::InstanceOf && *to == type_schema_entity_id
                })
        }))
    }

    /// Read beyond the bound by the overlay's possible removals, then correct.
    /// Saturate at `bound + 1`, matching the stored count's contract.
    async fn live_direct_dependents(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
        bound: usize,
    ) -> Result<usize, ScopeError> {
        let graph = self.graph().await;
        let touched = graph.touched();
        let mut live: HashSet<i64> = HashSet::new();
        if entity_id > 0 {
            let limit = bound.saturating_add(touched.len()).saturating_add(1);
            live.extend(
                self.base
                    .live_direct_dependent_ids(tx, scope, entity_id, None, limit)
                    .await?
                    .into_iter()
                    .filter(|id| !touched.contains(id)),
            );
        }
        for (from, edges) in graph.sources() {
            if graph.is_live(from) && edges.iter().any(|(_, to)| *to == entity_id) {
                live.insert(from);
            }
        }
        Ok(live.len().min(bound.saturating_add(1)))
    }

    async fn edge_page(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _entity_ids: &[i64],
        _side: EdgeSide,
        _after: Option<&DependencyEdgeRow>,
        _limit: usize,
    ) -> Result<Vec<DependencyEdgeRow>, ScopeError> {
        Err(unsupported(
            "an admission view is not pageable; it walks the base relation itself",
        ))
    }

    async fn live_direct_dependent_ids(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _entity_id: i64,
        _kind: Option<DependencyKind>,
        _limit: usize,
    ) -> Result<Vec<i64>, ScopeError> {
        Err(unsupported(
            "an admission view answers dependant questions whole, not as a bounded id page",
        ))
    }

    async fn edges_within(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<EntityEdge>, ScopeError> {
        self.walk_edges_within(tx, scope, entity_ids).await
    }

    async fn closure(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        roots: &[String],
    ) -> Result<DependencyClosure, ScopeError> {
        self.walk_closure(tx, scope, roots).await
    }

    async fn reverse_impact(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        roots: &[i64],
        write_set_bound: usize,
    ) -> Result<ReverseImpact, ScopeError> {
        self.walk_reverse_impact(tx, scope, roots, write_set_bound)
            .await
    }

    async fn replace_outgoing(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        from_entity_id: i64,
        edges: &[(DependencyKind, i64)],
    ) -> Result<(), ScopeError> {
        self.overlay()
            .await
            .replace_edges(from_entity_id, edges.to_vec());
        Ok(())
    }
}
