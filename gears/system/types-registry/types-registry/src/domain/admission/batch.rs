//! Shared batch traversal for persistent admission and dry-run prediction.
//!
//! Cycles are refused first, then candidates run in dependency order. Blocking
//! uses the outcome the executor actually returns: a committing pass may recover
//! an already-terminal item instead of recording the requested refusal.
//! Executors own transaction boundaries, retries and result persistence.

use std::collections::HashMap;
use std::future::Future;

use toolkit_db::DbTx;
use toolkit_db::secure::AccessScope;

use super::AdmissionFailureReason;
use super::errors::{ItemFailure, WorkerError};
use super::graph::{
    BatchCandidate, BatchOrder, BlockKind, Blocker, DependencyLink, order_batch,
    order_deletion_batch,
};
use crate::domain::enums::OperationItemStatus;
use crate::domain::ports::{OperationItemRow, Stores};

/// Process each candidate once, returning slots in submission order.
///
/// `execute` either evaluates the item or handles the supplied cycle/blocking
/// refusal. The graph partitions every index into cyclic or ordered candidates;
/// callers retain their existing missing-slot recovery if that invariant breaks.
pub(super) async fn run_ordered<T, F>(
    items: &[OperationItemRow],
    order: &BatchOrder,
    mut execute: impl FnMut(usize, Option<ItemFailure>) -> F,
    status: impl Fn(&T) -> OperationItemStatus,
) -> Result<Vec<Option<T>>, WorkerError>
where
    F: Future<Output = Result<T, WorkerError>>,
{
    let mut outcomes: Vec<Option<T>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    for member in order.cyclic() {
        let failure = ItemFailure::new(AdmissionFailureReason::InvalidSchema, member.message());
        outcomes[member.index] = Some(execute(member.index, Some(failure)).await?);
    }
    for &index in order.order() {
        let refusal = blocked_by(order, index, |i| outcomes[i].as_ref().map(&status))
            .map(|blocker| blocked_failure(blocker, &items[blocker.index].gts_id));
        outcomes[index] = Some(execute(index, refusal).await?);
    }
    Ok(outcomes)
}

/// Registration orders dependencies before the candidates that consume them.
pub(super) fn registration_order(items: &[OperationItemRow]) -> BatchOrder {
    let candidates: Vec<BatchCandidate> = items.iter().map(batch_candidate).collect();
    order_batch(&candidates)
}

/// The ordering's view of one stored item. An unparsable payload yields no
/// content and therefore no edge — the item's own evaluation refuses it with
/// `invalid_document`, which is a better message than anything this layer has.
fn batch_candidate(item: &OperationItemRow) -> BatchCandidate {
    BatchCandidate {
        gts_id: item.gts_id.clone(),
        content: item
            .request_payload
            .as_deref()
            .and_then(|payload| serde_json::from_str(payload).ok()),
    }
}

/// Order deletions within the caller's snapshot.
///
/// # Errors
/// Propagates either read failure.
pub(super) async fn order_deletions(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    gts_ids: &[String],
) -> Result<BatchOrder, WorkerError> {
    let rows = stores.find_by_gts_ids(tx, scope, gts_ids).await?;
    let entity_ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
    let named: HashMap<i64, String> = rows.into_iter().map(|row| (row.id, row.gts_id)).collect();
    let links = stores
        .edges_within(tx, scope, &entity_ids)
        .await?
        .into_iter()
        .filter_map(|edge| {
            Some(DependencyLink {
                dependant: named.get(&edge.from_entity_id)?.clone(),
                target: named.get(&edge.to_entity_id)?.clone(),
            })
        })
        .collect::<Vec<_>>();
    Ok(order_deletion_batch(gts_ids, &links))
}

/// Find the first failed in-batch blocker, or `None`.
/// Blocking propagates through failed outcomes in topological order; cycle
/// members are refused first. Only a self-blocker can have no outcome yet.
fn blocked_by(
    order: &BatchOrder,
    index: usize,
    decided: impl Fn(usize) -> Option<OperationItemStatus>,
) -> Option<Blocker> {
    order
        .blockers(index)
        .iter()
        .copied()
        .find(|blocker| decided(blocker.index) == Some(OperationItemStatus::Failed))
}

/// The refusal a blocked candidate carries, naming the candidate that blocked it.
fn blocked_failure(blocker: Blocker, target: &str) -> ItemFailure {
    let edge = match blocker.kind {
        BlockKind::Predecessor => "the preceding minor",
        BlockKind::Dependency => "the selected dependency",
    };
    ItemFailure::new(
        blocker.kind.reason(),
        format!(
            "{edge} '{target}' was submitted in the same batch and did not succeed, so this \
             candidate was not evaluated and nothing was committed for it"
        ),
    )
}
