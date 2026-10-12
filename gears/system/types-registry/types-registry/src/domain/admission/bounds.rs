//! What a candidate's resolution inputs must satisfy before it is resolved:
//! the per-document budgets, shared by admission and dependent refresh, and the
//! presence of everything it consumes.

use std::collections::HashSet;

use gts::{GtsId, GtsStore, ResolvedType};

use super::errors::ItemFailure;
use crate::config::Limits;
use crate::domain::admission::AdmissionFailureReason;
use crate::domain::artifacts::{MaterializedArtifacts, materialize};
use crate::domain::dependency::extract_edges;
use crate::domain::enums::DependencyKind;

/// Check a candidate's resolution inputs: that there are not too many of them,
/// and that every one it names is present.
///
/// **Two refusals, not one**, because a single walk discovers both and walking
/// twice would read the same documents to answer half the question each time:
/// [`AdmissionFailureReason::ResolutionClosureExceeded`] when the distinct
/// documents exceed `bound`, and [`ItemFailure::missing_dependency`]
/// (`dependency_not_found`) when an edge names a target the store does not hold.
/// The second is why this is not called `check_closure` any more — a
/// missing-dependency refusal coming out of a function named after a document
/// budget is easy to miss at the call sites.
///
/// Walk the authored documents in the overlaid store: committed outgoing edges
/// of a revised candidate may have been removed by this revision. Unrelated
/// documents in a shared refresh store do not consume this candidate's budget.
pub fn check_resolution_inputs(
    store: &mut GtsStore,
    root: &str,
    bound: usize,
) -> Result<(), ItemFailure> {
    let mut seen = HashSet::new();
    let mut pending = vec![root.to_owned()];
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if seen.len() > bound {
            return Err(ItemFailure::new(
                AdmissionFailureReason::ResolutionClosureExceeded,
                format!("resolution requires more than {bound} documents; nothing was committed"),
            ));
        }
        let Some(document) = store.get(&id) else {
            // Validation diagnoses an absent root; dependency targets are checked below.
            continue;
        };
        let parsed = GtsId::try_new(&id).map_err(|error| {
            ItemFailure::new(AdmissionFailureReason::InvalidSchema, error.to_string())
        })?;
        let mut edges = extract_edges(&parsed, &document.content).map_err(|error| {
            ItemFailure::new(AdmissionFailureReason::InvalidSchema, error.to_string())
        })?;
        // A base can also occur in allOf/$ref; report its semantic role first.
        edges.sort_by_key(|edge| edge.kind == DependencyKind::SchemaRef);
        for edge in edges {
            if store.get(&edge.target).is_none() {
                return Err(ItemFailure::missing_dependency(edge));
            }
            pending.push(edge.target);
        }
    }
    Ok(())
}

/// Apply the byte limit to each canonical effective document before persistence.
pub fn materialize_bounded(
    resolved: &ResolvedType,
    limits: &Limits,
) -> Result<MaterializedArtifacts, ItemFailure> {
    let artifacts = materialize(resolved);
    let bound = limits.resolved_document.bytes();
    for document in [
        &artifacts.resolved_schema,
        &artifacts.effective_traits,
        &artifacts.effective_traits_schema,
    ] {
        if document.len() > bound {
            return Err(ItemFailure::new(
                AdmissionFailureReason::ResolvedDocumentTooLarge,
                format!("a resolved document exceeds {bound} bytes; nothing was committed"),
            ));
        }
    }
    Ok(artifacts)
}
