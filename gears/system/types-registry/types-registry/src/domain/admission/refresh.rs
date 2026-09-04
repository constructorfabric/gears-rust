//! Transactional reverse-impact artifact refresh (SPEC §8.1 step 4.6, D5).

use std::collections::HashMap;

use time::OffsetDateTime;
use toolkit_db::DbTx;
use toolkit_db::secure::AccessScope;
use toolkit_macros::domain_model;

use super::errors::{ItemFailure, WorkerError};
use super::vector::VectorDrift;
use crate::domain::artifacts::{MaterializedArtifacts, materialize};
use crate::domain::enums::{EntityKind, LifecycleStatus};
use crate::domain::gts_store::{UnitDocument, load_unit_store};
use crate::domain::ports::{
    CurrentSchemaCas, CurrentTypeSchemaRow, EntityRow, NewCurrentTypeSchema, ReverseImpact, Stores,
};

/// What one refresh wrote.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefreshOutcome {
    /// The dependents whose artifacts were rewritten, `gts_id`-sorted.
    pub refreshed: Vec<String>,
    /// Dependents recomputed, including unchanged ones.
    pub examined: usize,
}

/// Re-materialize the effective artifacts of everything that transitively depends on `roots`.
pub async fn refresh_dependents(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    roots: &[i64],
    write_set_bound: usize,
    now: OffsetDateTime,
) -> Result<Result<RefreshOutcome, ItemFailure>, WorkerError> {
    let dependents = match stores
        .reverse_impact(tx, scope, roots, write_set_bound)
        .await?
    {
        ReverseImpact::Within(rows) => rows,
        // The refusal carries both operator context and a stable machine reason.
        ReverseImpact::OverBound { at_least, bound } => {
            return Ok(Err(ItemFailure::new(
                "activation_write_set_exceeded",
                format!(
                    "this revision reaches at least {at_least} dependents, over the \
                     configured activation write set bound of {bound}; the bound is on the \
                     set a revision reaches, of which what it rewrites is a subset; nothing \
                     was committed"
                ),
            )));
        }
    };

    // Instances have no materialized artifacts to refresh.
    let subjects: Vec<EntityRow> = dependents
        .into_iter()
        .filter(|row| {
            row.entity_kind == EntityKind::TypeSchema
                && row.lifecycle_status != LifecycleStatus::Deleted
        })
        .collect();
    if subjects.is_empty() {
        return Ok(Ok(RefreshOutcome::default()));
    }

    // Dependents share one transient store and much of its closure.
    let mut documents = Vec::with_capacity(subjects.len());
    let subject_ids: Vec<i64> = subjects.iter().map(|row| row.id).collect();
    // Carry the projection that supplied each document into the later CAS.
    let mut raw: HashMap<i64, (CurrentSchemaCas, String)> = stores
        .current_documents(tx, scope, &subject_ids)
        .await?
        .into_iter()
        .map(|doc| (doc.entity_id, (doc.projection, doc.raw_schema)))
        .collect();
    let mut token: HashMap<i64, CurrentSchemaCas> = HashMap::with_capacity(subjects.len());
    for row in &subjects {
        let Some((projection, text)) = raw.remove(&row.id) else {
            return Err(WorkerError::CurrentStateMissing {
                gts_id: row.gts_id.clone(),
                entity_id: row.id,
            });
        };
        let content = serde_json::from_str(&text).map_err(|source| {
            WorkerError::StoreBuild(crate::domain::gts_store::StoreBuildError::Content {
                gts_id: row.gts_id.clone(),
                source,
            })
        })?;
        token.insert(row.id, projection);
        documents.push(UnitDocument {
            gts_id: row.gts_id.clone(),
            content,
        });
    }

    let mut store = load_unit_store(stores, tx, scope, documents)
        .await
        .map_err(WorkerError::StoreBuild)?;

    // Keep CPU-bound materialization off Tokio workers while the transaction is open.
    let blocking_subjects: Vec<(i64, String)> = subjects
        .iter()
        .map(|row| (row.id, row.gts_id.clone()))
        .collect();
    let recomputed = tokio::task::spawn_blocking(move || {
        let mut artifacts = Vec::with_capacity(blocking_subjects.len());
        for (entity_id, gts_id) in blocking_subjects {
            let resolved = store.store_mut().validate_schema(&gts_id);
            let resolved = match resolved {
                Ok(resolved) => resolved,
                Err(error) => {
                    return Err(DependentInvalid {
                        entity_id,
                        gts_id,
                        error: error.to_string(),
                    });
                }
            };
            artifacts.push((entity_id, gts_id, materialize(&resolved)));
        }
        Ok(artifacts)
    })
    .await
    .map_err(WorkerError::EvaluationTask)?;
    let recomputed: Vec<(i64, String, MaterializedArtifacts)> = match recomputed {
        Ok(recomputed) => recomputed,
        // Committed content that no longer resolves against the new revision.
        Err(refusal) => {
            tracing::warn!(
                gts_id = %refusal.gts_id,
                entity_id = refusal.entity_id,
                error = %refusal.error,
                "types_registry dependent no longer validates against a new revision"
            );
            return Ok(Err(ItemFailure::new(
                "dependent_invalid",
                "a dependent of this candidate no longer validates against this \
                 revision; nothing was committed"
                    .to_owned(),
            )));
        }
    };

    // Batch the read to minimize time holding the candidate and family locks.
    let mut current: HashMap<i64, CurrentTypeSchemaRow> = stores
        .current_schemas(tx, scope, &subject_ids)
        .await?
        .into_iter()
        .map(|row| (row.entity_id, row))
        .collect();

    let mut refreshed = Vec::new();
    for (entity_id, gts_id, artifacts) in recomputed {
        let current =
            current
                .remove(&entity_id)
                .ok_or_else(|| WorkerError::CurrentStateMissing {
                    gts_id: gts_id.clone(),
                    entity_id,
                })?;
        // Even a skipped write must be based on the projection that supplied the document.
        let Some(expected) = token.remove(&entity_id) else {
            return Err(WorkerError::CurrentStateMissing {
                gts_id: gts_id.clone(),
                entity_id,
            });
        };
        let moved = expected.revision_no != current.revision_no
            || expected.resolution_fingerprint != current.resolution_fingerprint;
        if moved {
            return Err(WorkerError::RevalidationRequired(
                VectorDrift::CurrentProjectionMoved {
                    gts_id: gts_id.clone(),
                },
            ));
        }
        if current.resolution_fingerprint == artifacts.resolution_fingerprint {
            continue;
        }

        // Refresh artifacts without moving the authored revision; a CAS miss is drift.
        if !stores
            .update_current_schema(
                tx,
                scope,
                NewCurrentTypeSchema {
                    entity_id,
                    revision_no: current.revision_no,
                    resolved_schema: artifacts.resolved_schema,
                    effective_traits: artifacts.effective_traits,
                    effective_traits_schema: artifacts.effective_traits_schema,
                    resolution_fingerprint: artifacts.resolution_fingerprint,
                    now,
                },
                expected,
            )
            .await?
        {
            return Err(WorkerError::RevalidationRequired(
                VectorDrift::CurrentProjectionMoved {
                    gts_id: gts_id.clone(),
                },
            ));
        }
        refreshed.push(gts_id);
    }

    Ok(Ok(RefreshOutcome {
        refreshed,
        examined: subjects.len(),
    }))
}

/// The blocking task's refusal: which dependent stopped validating, and why.
struct DependentInvalid {
    entity_id: i64,
    gts_id: String,
    error: String,
}
