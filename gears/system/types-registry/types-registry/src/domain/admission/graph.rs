//! Pure batch ordering (T19, SPEC §8.1 steps 1–2).
//!
//! Ordering uses `$ref`, derivation, Instance conformance and implicit minor
//! predecessor edges. Predecessor edges are never persisted or returned by
//! [`extract_edges`] (SPEC §3.2).
//!
//! Cycle detection uses `$ref` and derivation, the edges effective forms inline.
//! The candidate overlay can introduce cycles even though committed state is
//! acyclic. The worker refuses cycle members with `invalid_schema` (ADR-0012).

use std::collections::{BTreeMap, BTreeSet};

use gts::GtsId;
use serde_json::Value;
use toolkit_macros::domain_model;

use crate::domain::admission::AdmissionFailureReason;
use crate::domain::dependency::extract_edges;
use crate::domain::enums::DependencyKind;
use crate::domain::family::{VersionProbe, version_probe};

/// One candidate as the ordering sees it.
///
/// Deliberately not an `OperationItemRow`: the ordering must be callable from a
/// unit test with no database, and the two fields below are everything it reads.
#[domain_model]
#[derive(Clone, Debug)]
pub struct BatchCandidate {
    pub gts_id: String,
    /// The authored document. `None` for a deletion, which submits none (T20);
    /// the identifier-derived edges still apply.
    pub content: Option<Value>,
}

/// Which edge a blocked candidate was waiting on, and therefore which refusal
/// reason it carries.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BlockKind {
    /// The implicit `vM.(n-1)~ → vM.n~` edge. Ordered first so that a candidate
    /// blocked both ways reports the stronger constraint: without its
    /// predecessor the identifier itself is inadmissible, and `missing_predecessor`
    /// is what the candidate would earn on its own.
    Predecessor,
    /// A selected dependency: an authored `$ref`, the derivation base, or the
    /// Instance's conformance target.
    Dependency,
}

impl BlockKind {
    /// The refusal a candidate carries when a blocker of this kind failed.
    #[must_use]
    pub const fn reason(self) -> AdmissionFailureReason {
        match self {
            Self::Predecessor => AdmissionFailureReason::BlockedByPredecessor,
            Self::Dependency => AdmissionFailureReason::BlockedByDependency,
        }
    }
}

/// One in-batch edge, from the waiting candidate's point of view.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blocker {
    /// Index of the candidate that must reach a terminal outcome first.
    pub index: usize,
    pub kind: BlockKind,
}

/// Which check refused a candidate's participation in the order.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CycleKind {
    /// A cycle over `$ref` and derivation — the edges an effective form inlines,
    /// so the candidate has no resolved form at all.
    Inlined,
    /// No inlined cycle, yet no topological order exists: the remaining edges
    /// close a loop through conformance or the predecessor edge. Refused rather
    /// than ordered arbitrarily.
    Unorderable,
}

/// A candidate the ordering refused, with the loop that refused it.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CyclicCandidate {
    pub index: usize,
    pub kind: CycleKind,
    /// Every member of the loop, `gts_id`-sorted so the message is the same for
    /// each member and stable across runs.
    pub cycle: Vec<String>,
}

impl CyclicCandidate {
    /// The refusal text, naming the whole loop rather than one edge of it: an
    /// operator has to break the cycle somewhere, and only the full member list
    /// says where the choices are.
    #[must_use]
    pub fn message(&self) -> String {
        let members = self.cycle.join(", ");
        match self.kind {
            CycleKind::Inlined => format!(
                "these candidates reference each other in a cycle over $ref and derivation, \
                 so none of them has a resolved form: {members}"
            ),
            CycleKind::Unorderable => format!(
                "these candidates cannot be put in an admission order — the batch closes a \
                 loop through conformance or the preceding minor: {members}"
            ),
        }
    }
}

/// What ordering one candidate set produced.
///
/// Indices throughout are positions in the slice passed to [`order_batch`], so
/// the caller keeps whatever it associated with each candidate.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchOrder {
    order: Vec<usize>,
    cyclic: Vec<CyclicCandidate>,
    blockers: Vec<Vec<Blocker>>,
}

impl BatchOrder {
    /// Processing order: every candidate that is not a cycle member, each one
    /// after everything it waits on.
    #[must_use]
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// The refused candidates, in input order.
    #[must_use]
    pub fn cyclic(&self) -> &[CyclicCandidate] {
        &self.cyclic
    }

    /// What this candidate waits on, `Predecessor` first and then by index — a
    /// total order, so the reason a blocked candidate reports does not depend on
    /// which blocker happened to be discovered first.
    ///
    /// # Panics
    /// If `index` is not a position in the ordered candidate set.
    #[must_use]
    pub fn blockers(&self, index: usize) -> &[Blocker] {
        &self.blockers[index]
    }
}

/// One stored dependency edge, as the deletion order reads it.
///
/// Identifiers rather than entity ids, so the ordering stays a pure function
/// over names and its tests need no database — the same property the
/// registration order has.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyLink {
    /// The entity that consumes `target`, and therefore must be deleted first.
    pub dependant: String,
    pub target: String,
}

/// Order deletion candidates dependant-first using stored dependency edges.
///
/// Place every candidate once. Leave external dependants and failed deletions
/// to the commit-time `has_registered_dependents` check; ordering adds no refusals
/// or blocking. Unsortable candidates indicate corrupt committed state and are
/// appended in submission order for individual evaluation.
#[must_use]
pub fn order_deletion_batch(gts_ids: &[String], edges: &[DependencyLink]) -> BatchOrder {
    let count = gts_ids.len();
    let mut position: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, gts_id) in gts_ids.iter().enumerate() {
        position.entry(gts_id.as_str()).or_insert(index);
    }

    // `waits_on[target]` is its in-batch dependants: the target is deleted last.
    let mut waits_on: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); count];
    for edge in edges {
        let (Some(&dependant), Some(&target)) = (
            position.get(edge.dependant.as_str()),
            position.get(edge.target.as_str()),
        ) else {
            continue;
        };
        if dependant != target {
            waits_on[target].insert(dependant);
        }
    }

    let blockers: Vec<Vec<Blocker>> = waits_on
        .iter()
        .map(|dependants| {
            dependants
                .iter()
                .map(|&index| Blocker {
                    index,
                    // The kind is unused here: nothing blocks in a deletion
                    // order. `Blocker` is reused only to share `topological`.
                    kind: BlockKind::Dependency,
                })
                .collect()
        })
        .collect();

    let (mut order, unplaced) = topological(&blockers, &BTreeSet::new(), count);
    if !unplaced.is_empty() {
        tracing::warn!(
            unplaced = unplaced.len(),
            "types_registry deletion batch holds a dependency cycle, which committed state \
             cannot: appending its members in submission order"
        );
        order.extend(unplaced);
    }
    BatchOrder {
        order,
        // Nothing is refused and nothing blocks here; see this function's
        // documentation. The empty vectors let the worker's loop stay one loop.
        cyclic: Vec::new(),
        blockers: vec![Vec::new(); count],
    }
}

/// Order one candidate set, reporting the cycles it cannot order.
///
/// Total: a candidate whose identifier does not parse, or whose content is not
/// the shape its identifier implies, contributes no edge and is still ordered.
/// Dropping it would lose the outcome the operation owes it; its own evaluation
/// is what refuses it.
#[must_use]
pub fn order_batch(candidates: &[BatchCandidate]) -> BatchOrder {
    let count = candidates.len();
    // First position wins a repeated identifier. Acceptance refuses duplicates
    // (`AcceptanceError::DuplicateCandidate`), so this is a fallback, not a rule.
    let mut position: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        position.entry(candidate.gts_id.as_str()).or_insert(index);
    }

    // `waits_on[i]` is the ordering graph; `inlined[i]` the cycle-bearing one.
    let mut waits_on: Vec<BTreeSet<(BlockKind, usize)>> = vec![BTreeSet::new(); count];
    let mut inlined: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (index, candidate) in candidates.iter().enumerate() {
        let Ok(id) = GtsId::try_new(&candidate.gts_id) else {
            continue;
        };
        if let Some(content) = &candidate.content
            && let Ok(edges) = extract_edges(&id, content)
        {
            for edge in edges {
                let Some(&target) = position.get(edge.target.as_str()) else {
                    continue;
                };
                waits_on[index].insert((BlockKind::Dependency, target));
                if matches!(
                    edge.kind,
                    DependencyKind::SchemaRef | DependencyKind::Derivation
                ) {
                    inlined[index].push(target);
                }
            }
        }
        // The implicit `vM.(n-1)~ → vM.n~` edge. Ordering only: it is never an
        // extracted edge and never reaches the `dependency` table.
        if let Some(VersionProbe::LaterMinor { predecessor, .. }) = version_probe(&id)
            && let Some(&target) = position.get(predecessor.as_str())
        {
            waits_on[index].insert((BlockKind::Predecessor, target));
        }
    }

    // One blocker per target, `Predecessor` beating `Dependency` for the same
    // one: the set is sorted by `(kind, index)`, so the first entry per target wins.
    let blockers: Vec<Vec<Blocker>> = waits_on
        .iter()
        .map(|edges| {
            let mut seen = BTreeSet::new();
            edges
                .iter()
                .filter(|&&(_, index)| seen.insert(index))
                .map(|&(kind, index)| Blocker { index, kind })
                .collect()
        })
        .collect();

    let mut cyclic = cycle_members(&inlined, candidates, CycleKind::Inlined);
    let mut refused: BTreeSet<usize> = cyclic.iter().map(|member| member.index).collect();
    let (mut order, unorderable) = topological(&blockers, &refused, count);
    if !unorderable.is_empty() {
        let cycles = unorderable_cycles(&unorderable, &blockers, candidates);
        refused.extend(cycles.iter().map(|member| member.index));
        cyclic.extend(cycles);
        // Kahn's residue also contains downstream candidates. Once the actual
        // cycle members are refused, place their dependants for blocked outcomes.
        (order, _) = topological(&blockers, &refused, count);
    }
    cyclic.sort_by_key(|member| member.index);

    BatchOrder {
        order,
        cyclic,
        blockers,
    }
}

/// Every candidate on a cycle, grouped by strongly connected component.
fn cycle_members(
    adjacency: &[Vec<usize>],
    candidates: &[BatchCandidate],
    kind: CycleKind,
) -> Vec<CyclicCandidate> {
    let mut members = Vec::new();
    for component in strongly_connected(adjacency) {
        // A one-node component is a cycle only through a self-referential edge,
        // which `strongly_connected` cannot distinguish from an ordinary node.
        let is_cycle = component.len() > 1
            || component
                .first()
                .is_some_and(|&only| adjacency[only].contains(&only));
        if !is_cycle {
            continue;
        }
        members.extend(describe(&component, candidates, kind));
    }
    members
}

/// Cycles in the residual ordering graph, excluding already refused inlined
/// cycles and the acyclic candidates that merely depend on an ordering cycle.
fn unorderable_cycles(
    remaining: &[usize],
    blockers: &[Vec<Blocker>],
    candidates: &[BatchCandidate],
) -> Vec<CyclicCandidate> {
    let remaining: BTreeSet<usize> = remaining.iter().copied().collect();
    let mut adjacency = vec![Vec::new(); candidates.len()];
    for &index in &remaining {
        adjacency[index] = blockers[index]
            .iter()
            .filter(|blocker| remaining.contains(&blocker.index))
            .map(|blocker| blocker.index)
            .collect();
    }
    cycle_members(&adjacency, candidates, CycleKind::Unorderable)
}

/// One [`CyclicCandidate`] per member, each carrying the same sorted member list.
fn describe(
    component: &[usize],
    candidates: &[BatchCandidate],
    kind: CycleKind,
) -> Vec<CyclicCandidate> {
    let mut cycle: Vec<String> = component
        .iter()
        .map(|&index| candidates[index].gts_id.clone())
        .collect();
    cycle.sort();
    component
        .iter()
        .map(|&index| CyclicCandidate {
            index,
            kind,
            cycle: cycle.clone(),
        })
        .collect()
}

/// Kahn's algorithm over the ordering graph, skipping `refused`.
///
/// Returns the order and whatever could not be placed. The ready set is a
/// `BTreeSet`, so ties break on the lowest index and the order is the same on
/// every run — a batch whose outcome depended on hash iteration would be a batch
/// whose outcome varied between pods.
fn topological(
    blockers: &[Vec<Blocker>],
    refused: &BTreeSet<usize>,
    count: usize,
) -> (Vec<usize>, Vec<usize>) {
    let mut waiting = vec![0usize; count];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); count];
    for index in (0..count).filter(|index| !refused.contains(index)) {
        for blocker in &blockers[index] {
            if refused.contains(&blocker.index) || blocker.index == index {
                continue;
            }
            waiting[index] += 1;
            dependents[blocker.index].push(index);
        }
    }

    let mut ready: BTreeSet<usize> = (0..count)
        .filter(|index| !refused.contains(index) && waiting[*index] == 0)
        .collect();
    let mut order = Vec::with_capacity(count - refused.len());
    while let Some(&index) = ready.iter().next() {
        ready.remove(&index);
        order.push(index);
        for &dependent in &dependents[index] {
            waiting[dependent] -= 1;
            if waiting[dependent] == 0 {
                ready.insert(dependent);
            }
        }
    }

    let unplaced = (0..count)
        .filter(|index| !refused.contains(index) && waiting[*index] > 0)
        .collect();
    (order, unplaced)
}

/// Tarjan's strongly-connected components, iteratively.
///
/// Iterative because the recursion depth would be the batch size, and the batch
/// size is operator-configurable (`limits.batch_candidates`): a call stack is not
/// a safe place to keep an input.
fn strongly_connected(adjacency: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNVISITED: usize = usize::MAX;
    let count = adjacency.len();
    let mut index = vec![UNVISITED; count];
    let mut low = vec![0usize; count];
    let mut on_stack = vec![false; count];
    let mut component_stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;
    let mut components = Vec::new();
    // (node, index of the next edge to walk) — the explicit call stack.
    let mut calls: Vec<(usize, usize)> = Vec::new();

    for root in 0..count {
        if index[root] != UNVISITED {
            continue;
        }
        calls.push((root, 0));
        while let Some((node, edge)) = calls.pop() {
            if edge == 0 {
                index[node] = next_index;
                low[node] = next_index;
                next_index += 1;
                component_stack.push(node);
                on_stack[node] = true;
            }
            if let Some(&next) = adjacency[node].get(edge) {
                calls.push((node, edge + 1));
                if index[next] == UNVISITED {
                    calls.push((next, 0));
                } else if on_stack[next] {
                    low[node] = low[node].min(index[next]);
                }
                continue;
            }
            if low[node] == index[node] {
                let mut component = Vec::new();
                while let Some(member) = component_stack.pop() {
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                components.push(component);
            }
            if let Some(&(parent, _)) = calls.last() {
                low[parent] = low[parent].min(low[node]);
            }
        }
    }
    components
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod graph_tests;
