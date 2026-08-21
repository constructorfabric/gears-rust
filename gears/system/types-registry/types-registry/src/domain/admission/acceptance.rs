//! The synchronous acceptance path (SPEC §8.1 steps 1–8).
//!
//! Two halves, deliberately separated. [`validate`] is a pure function of the
//! request and the configuration: it has no database in scope, which is how the
//! ordering invariant *"the policy gate precedes any existence lookup"* is kept
//! structurally rather than by review. [`accept`] then resolves the
//! `Idempotency-Key` and commits one operation, its items and the outbox message
//! in a single transaction.
//!
//! # Which steps live here
//!
//! | Step | Where |
//! |---|---|
//! | 1 envelope and batch size | here |
//! | 2 candidate identifiers | here |
//! | 3 registration policy | here (via [`RegistrationPolicy`]) |
//! | 4 managed identifier profile | here |
//! | 5 declared dialect | here |
//! | 6 `force` | here |
//! | 7 ADR-0015 major-0 quarantine | **T18** — it needs the reference extractor |
//! | 8 canonicalize, fingerprint, idempotency | here |
//!
//! Step 7 is a gap by dependency: it refuses a stable candidate whose base, `$ref`
//! or `x-gts-ref` targets include a major-0 identifier, and the extractor that
//! finds those targets is T13's. A `TODO` marks its position between steps 6 and 8,
//! so it lands as an insertion rather than a reordering.

use std::collections::BTreeSet;
use std::sync::Arc;

use gts::{GtsId, GtsIdSegment};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, ScopeError};
use toolkit_db::{DBProvider, DbError};
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::fingerprint::{
    FingerprintCandidate, FingerprintInput, P0_PRINCIPAL_ID, RequestFingerprint, ScopeHash,
    canonical_text, idempotency_scope_hash, request_fingerprint,
};
use super::{Accepted, OperationDispatch, Precondition, SubmitRequest};
use crate::config::TypesRegistryConfig;
use crate::domain::enums::{OperationKind, OwnershipScope, Plane};
use crate::domain::policy::{PolicyRefusal, RegistrationPolicy};
use crate::domain::ports::{NewOperation, NewOperationItem, OperationRow, Stores};

/// Largest `Idempotency-Key` the column accepts (`varchar(255)`).
pub(crate) const MAX_IDEMPOTENCY_KEY: usize = 255;

/// The canonical Draft-07 dialect, and the closed set that normalizes onto it
/// (ADR-0014, SPEC §8.1 step 5).
const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";
const DRAFT_07_SPELLINGS: [&str; 4] = [
    "http://json-schema.org/draft-07/schema#",
    "http://json-schema.org/draft-07/schema",
    "https://json-schema.org/draft-07/schema#",
    "https://json-schema.org/draft-07/schema",
];

/// Why a request is refused before it becomes an operation.
///
/// One variant per reason, so T16 can count them separately: a single
/// `Refused(String)` would make "refusals by reason" a log-parsing exercise.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum AcceptanceError {
    #[error("an Idempotency-Key is required")]
    MissingIdempotencyKey,
    #[error("the Idempotency-Key is longer than {MAX_IDEMPOTENCY_KEY} characters")]
    IdempotencyKeyTooLong { length: usize },
    #[error("a request must carry at least one candidate")]
    EmptyBatch,
    #[error("{count} candidates exceeds limits.batch_candidates ({limit})")]
    BatchTooLarge { count: usize, limit: usize },
    #[error("'{gts_id}' is not a canonical GTS identifier: {reason}")]
    InvalidIdentifier { gts_id: String, reason: String },
    #[error("'{gts_id}' appears twice in one batch")]
    DuplicateCandidate { gts_id: String },
    #[error("{0}")]
    PolicyRefused(#[source] PolicyRefusalError),
    #[error("'{gts_id}' carries an explicit UUID tail, which is not registrable")]
    ExplicitUuidTail { gts_id: String },
    #[error("registered Instance '{gts_id}' must name a stable version: {reason}")]
    InstanceVersionProfile { gts_id: String, reason: String },
    #[error("'{gts_id}' declares no top-level $schema")]
    MissingDialect { gts_id: String },
    #[error("'{gts_id}' declares dialect '{found}', which is not the Draft-07 spelling set")]
    UnsupportedDialect { gts_id: String, found: String },
    #[error("'{gts_id}' declares a differing $schema at '{path}'")]
    ConflictingDialect { gts_id: String, path: String },
    #[error("'{gts_id}' carries no document, which a registration requires")]
    MissingContent { gts_id: String },
    #[error("'{gts_id}' is {size} bytes, over limits.authored_document ({limit})")]
    AuthoredDocumentTooLarge {
        gts_id: String,
        size: usize,
        limit: usize,
    },
    #[error("force on '{gts_id}' is refused: allow_compatibility_force is off")]
    ForceNotPermitted { gts_id: String },
    #[error("force on '{gts_id}' is refused: it has no cross-minor check to waive")]
    ForceHasNothingToWaive { gts_id: String },
    #[error(
        "expected_resource_version 0 on '{gts_id}' is refused: omit the field to require absence"
    )]
    ZeroPrecondition { gts_id: String },
    #[error("expected_resource_version {version} on '{gts_id}' is negative")]
    NegativePrecondition { gts_id: String, version: i64 },
    #[error(
        "expected_resource_version {version} on '{gts_id}' is refused: \
         content revisions arrive with T11"
    )]
    RevisionNotAccepted { gts_id: String, version: i64 },
    #[error("operation kind is not accepted yet: deletion arrives with T20")]
    UnsupportedOperationKind,
    #[error("dry_run is not accepted yet: rollback-only evaluation arrives with T20")]
    DryRunNotAccepted,
    /// The `409` case: the key exists with a different request behind it.
    #[error(
        "Idempotency-Key is already bound to operation {operation_id} with a different request"
    )]
    FingerprintConflict { operation_id: Uuid },
    #[error("dispatching the operation failed: {0}")]
    Dispatch(#[source] anyhow::Error),
    #[error("storage failure during acceptance: {0}")]
    Storage(#[from] ScopeError),
    #[error("database failure during acceptance: {0}")]
    Db(#[from] DbError),
}

/// Wrapper so [`PolicyRefusal`] — which is a value, not an error — can be a
/// `#[source]` without implementing `Error` in the policy module.
#[domain_model]
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PolicyRefusalError(pub PolicyRefusal);

/// What acceptance needs besides the request: the compiled policy and the limits.
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct AcceptanceContext<'a> {
    pub policy: &'a RegistrationPolicy,
    pub config: &'a TypesRegistryConfig,
}

/// A validated request: everything the transaction needs, and nothing that would
/// require another look at the request.
#[domain_model]
#[derive(Clone, Debug)]
pub struct Validated {
    pub kind: OperationKind,
    pub dry_run: bool,
    pub idempotency_key: String,
    pub idempotency_scope_hash: ScopeHash,
    pub request_fingerprint: RequestFingerprint,
    pub items: Vec<NewOperationItem>,
}

/// Steps 1–6 and 8, in that order, with no database in scope.
///
/// # Errors
/// One [`AcceptanceError`] per refusal reason.
pub fn validate(
    ctx: &AcceptanceContext<'_>,
    request: &SubmitRequest,
) -> Result<Validated, AcceptanceError> {
    // --- step 1: envelope and batch size ---------------------------------
    let key = request.idempotency_key.trim();
    if key.is_empty() {
        return Err(AcceptanceError::MissingIdempotencyKey);
    }
    if key.len() > MAX_IDEMPOTENCY_KEY {
        return Err(AcceptanceError::IdempotencyKeyTooLong { length: key.len() });
    }
    if request.kind != OperationKind::Registration {
        // Deletion has its own short protocol and its own precondition rule
        // (T20). Refusing loudly beats accepting an operation whose rules are not
        // implemented, which would fail later in the worker with a worse message.
        return Err(AcceptanceError::UnsupportedOperationKind);
    }
    if request.dry_run {
        // Dry Run needs a rollback-only evaluation transaction and a separate
        // terminal-outcome write (T20). Letting it reach the ordinary creation
        // worker makes `mark_item_succeeded` violate the dry-run result-column
        // CHECK and strands the accepted operation in `running`.
        return Err(AcceptanceError::DryRunNotAccepted);
    }
    if request.candidates.is_empty() {
        return Err(AcceptanceError::EmptyBatch);
    }
    let limit = ctx.config.limits.batch_candidates;
    if request.candidates.len() > limit {
        return Err(AcceptanceError::BatchTooLarge {
            count: request.candidates.len(),
            limit,
        });
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut items = Vec::with_capacity(request.candidates.len());
    let mut fingerprint_forces = Vec::with_capacity(request.candidates.len());

    for (index, candidate) in request.candidates.iter().enumerate() {
        // --- step 2: candidate identifiers -------------------------------
        let id =
            GtsId::try_new(&candidate.gts_id).map_err(|e| AcceptanceError::InvalidIdentifier {
                gts_id: candidate.gts_id.clone(),
                reason: e.to_string(),
            })?;
        if id.id() != candidate.gts_id {
            // `try_new` trims and normalizes. A spelling that changed is not
            // refused for being wrong but for being ambiguous: two spellings of
            // one identifier in a batch would fingerprint differently while
            // naming the same entity.
            return Err(AcceptanceError::InvalidIdentifier {
                gts_id: candidate.gts_id.clone(),
                reason: format!("canonical form is '{}'", id.id()),
            });
        }
        if !seen.insert(id.id().to_owned()) {
            return Err(AcceptanceError::DuplicateCandidate {
                gts_id: id.id().to_owned(),
            });
        }

        // --- preconditions ------------------------------------------------
        let expected = match candidate.expected_resource_version {
            None => Precondition::MustNotExist,
            Some(0) => {
                return Err(AcceptanceError::ZeroPrecondition {
                    gts_id: id.id().to_owned(),
                });
            }
            Some(v) if v < 0 => {
                return Err(AcceptanceError::NegativePrecondition {
                    gts_id: id.id().to_owned(),
                    version: v,
                });
            }
            // A positive precondition claims the candidate is a revision, and
            // nothing in P0 can check the claim: `commit_creation` ignores the field,
            // so it would commit as an ordinary creation at `resource_version = 1` —
            // with step 3 skipped, because step 3 is skipped for revisions. Naming a
            // version would then be enough to register inside a region the deployment
            // closes. Refused loudly until T11, as deletion is until T20.
            Some(v) => {
                return Err(AcceptanceError::RevisionNotAccepted {
                    gts_id: id.id().to_owned(),
                    version: v,
                });
            }
        };

        // --- step 3: registration policy ---------------------------------
        // Unconditional, because every candidate reaching this line is a declared
        // creation: a positive `expected_resource_version` was refused above and
        // deletion was refused with the envelope.
        //
        // TODO(T11): a revision — and at T20 a deletion — must bypass this gate
        // again, so that closing a region cannot freeze the entities already in it
        // (SPEC §8.1 step 3, DESIGN §3.2). Restore the condition *together with*
        // the precondition check in the commit transaction: gating on the
        // caller's declared kind alone is the bypass this refusal replaces.
        ctx.policy
            .admits(&id, OwnershipScope::Global)
            .map_err(|refusal| AcceptanceError::PolicyRefused(PolicyRefusalError(refusal)))?;

        // --- step 4: managed identifier profile --------------------------
        if id
            .segments()
            .iter()
            .any(|segment| segment.uuid_tail().is_some())
        {
            return Err(AcceptanceError::ExplicitUuidTail {
                gts_id: id.id().to_owned(),
            });
        }
        if !id.is_type() {
            // A registered Instance. Its last segment must name a stable major
            // and carry no minor (ADR-0004, ADR-0015); on a Type Schema a minor
            // is admissible under any prefix.
            let last = id.segments().last();
            let reason = match (
                last.and_then(GtsIdSegment::ver_major_opt),
                last.and_then(GtsIdSegment::ver_minor),
            ) {
                (Some(0), _) => Some("major 0 is quarantined".to_owned()),
                (_, Some(minor)) => Some(format!("it carries minor {minor}")),
                _ => None,
            };
            if let Some(reason) = reason {
                return Err(AcceptanceError::InstanceVersionProfile {
                    gts_id: id.id().to_owned(),
                    reason,
                });
            }
        }

        // --- step 5: declared dialect ------------------------------------
        let content =
            candidate
                .content
                .as_ref()
                .ok_or_else(|| AcceptanceError::MissingContent {
                    gts_id: id.id().to_owned(),
                })?;
        if id.is_type() {
            check_dialect(id.id(), content)?;
        }

        // --- step 6: force ------------------------------------------------
        if candidate.force {
            if !ctx.config.allow_compatibility_force {
                return Err(AcceptanceError::ForceNotPermitted {
                    gts_id: id.id().to_owned(),
                });
            }
            if !has_cross_minor_check(&id) {
                return Err(AcceptanceError::ForceHasNothingToWaive {
                    gts_id: id.id().to_owned(),
                });
            }
        }

        // TODO(T18): step 7, the ADR-0015 quarantine — refuse a stable candidate
        // whose immediate base, `$ref` or `x-gts-ref` targets include a major-0
        // identifier. It needs T13's reference extractor, so it slots in here
        // rather than being reordered in later.

        // --- step 8: canonicalize ----------------------------------------
        let canonical = canonical_text(content);
        let authored_limit = ctx.config.limits.authored_document.bytes();
        if canonical.len() > authored_limit {
            return Err(AcceptanceError::AuthoredDocumentTooLarge {
                gts_id: id.id().to_owned(),
                size: canonical.len(),
                limit: authored_limit,
            });
        }

        let item_no = i32::try_from(index).map_err(|_| AcceptanceError::BatchTooLarge {
            count: request.candidates.len(),
            limit,
        })?;
        fingerprint_forces.push(candidate.force);
        items.push(NewOperationItem {
            item_no,
            gts_id: id.id().to_owned(),
            precondition: expected,
            request_payload: canonical,
        });
    }

    let fingerprint_candidates: Vec<FingerprintCandidate<'_>> = items
        .iter()
        .zip(&fingerprint_forces)
        .map(|(item, force)| FingerprintCandidate {
            gts_id: &item.gts_id,
            canonical_body: &item.request_payload,
            precondition: item.precondition,
            force: *force,
        })
        .collect();

    Ok(Validated {
        kind: request.kind,
        dry_run: request.dry_run,
        idempotency_key: key.to_owned(),
        // ponytail: ceiling C2 — the three inputs are constants in P0, so the key
        // namespace is global. See `fingerprint::P0_PRINCIPAL_ID`.
        idempotency_scope_hash: idempotency_scope_hash(Plane::Platform, None, P0_PRINCIPAL_ID),
        request_fingerprint: request_fingerprint(&FingerprintInput {
            kind: request.kind,
            dry_run: request.dry_run,
            plane: Plane::Platform,
            tenant_id: None,
            principal_id: P0_PRINCIPAL_ID,
            ownership_scope: OwnershipScope::Global,
            candidates: &fingerprint_candidates,
        }),
        items,
    })
}

/// Accept a request: validate, resolve the `Idempotency-Key`, and commit the
/// operation, its items and the outbox message in one transaction.
///
/// Reads no entity state — not `entity`, not `version_family`, not
/// `type_schema`. Every existence question belongs to the worker under its locks.
///
/// # Errors
/// Any [`AcceptanceError`], including [`AcceptanceError::FingerprintConflict`]
/// for a key already bound to a different request.
pub async fn accept(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<AcceptanceError>,
    scope: &AccessScope,
    ctx: &AcceptanceContext<'_>,
    dispatch: &Arc<dyn OperationDispatch>,
    request: &SubmitRequest,
    now: OffsetDateTime,
) -> Result<Accepted, AcceptanceError> {
    let validated = Arc::new(validate(ctx, request)?);

    // Replay before insert: the common case for a retrying client, and one read
    // against `operation` rather than a failed insert.
    if let Some(existing) = find_operation_by_key(stores, db, scope, &validated).await? {
        return resolve_replay(&existing, &validated.request_fingerprint);
    }

    let operation_id = Uuid::new_v4();
    // The transaction closure is quantified over any transaction lifetime, so its
    // future may not borrow anything shorter-lived than `'static`. Both of these
    // are cheap clones — `AccessScope` is a constraint list and the dispatcher is
    // behind an `Arc`.
    let tx_scope = scope.clone();
    let tx_dispatch = Arc::clone(dispatch);
    let tx_stores = Arc::clone(stores);
    let insert = db
        .transaction(|tx| {
            let validated = Arc::clone(&validated);
            let tx_scope = tx_scope.clone();
            let tx_dispatch = Arc::clone(&tx_dispatch);
            let tx_stores = Arc::clone(&tx_stores);
            Box::pin(async move {
                let parent = tx_stores
                    .insert_operation(
                        tx,
                        &tx_scope,
                        NewOperation {
                            id: operation_id,
                            kind: validated.kind,
                            dry_run: validated.dry_run,
                            // Every P0 operation is platform-plane. ponytail: ceiling
                            // C8 — the plane is expressed by this column and the
                            // contract, not enforced by the transport.
                            plane: Plane::Platform,
                            tenant_id: None,
                            principal_id: P0_PRINCIPAL_ID,
                            idempotency_key: validated.idempotency_key.clone(),
                            idempotency_scope_hash: validated.idempotency_scope_hash,
                            request_fingerprint: validated.request_fingerprint,
                            now,
                        },
                    )
                    .await?;
                tx_stores
                    .insert_items(tx, &tx_scope, &parent, &validated.items)
                    .await?;
                tx_dispatch
                    .enqueue(tx, parent.id)
                    .await
                    .map_err(AcceptanceError::Dispatch)?;
                Ok(Accepted {
                    operation_id: parent.id,
                    replayed: false,
                    status: parent.status,
                })
            })
        })
        .await;

    match insert {
        Ok(accepted) => Ok(accepted),
        // The unique constraint on (idempotency_scope_hash, idempotency_key) is the
        // serialization point between two concurrent acceptances — this layer has no
        // row to lock, and the read above cannot close the window. The loser re-reads
        // the winner outside the rolled-back transaction; see `load_replay`.
        Err(AcceptanceError::Storage(e)) if e.is_unique_violation() => {
            let winner = find_operation_by_key(stores, db, scope, &validated)
                .await?
                .ok_or(AcceptanceError::Storage(ScopeError::Invalid(
                    "operation vanished between insert and re-read",
                )))?;
            resolve_replay(&winner, &validated.request_fingerprint)
        }
        Err(e) => Err(e),
    }
}

/// A stored operation under this key: a replay when the fingerprint matches, a
/// `409` when it does not.
fn resolve_replay(
    existing: &OperationRow,
    fingerprint: &RequestFingerprint,
) -> Result<Accepted, AcceptanceError> {
    if &existing.request_fingerprint == fingerprint {
        Ok(Accepted {
            operation_id: existing.id,
            replayed: true,
            status: existing.status,
        })
    } else {
        Err(AcceptanceError::FingerprintConflict {
            operation_id: existing.id,
        })
    }
}

/// Step 5. A top-level `$schema` in the closed Draft-07 set, and no differing
/// `$schema` below the root.
fn check_dialect(gts_id: &str, content: &Value) -> Result<(), AcceptanceError> {
    let declared = content
        .get("$schema")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AcceptanceError::MissingDialect {
            gts_id: gts_id.to_owned(),
        })?;
    if normalize_dialect(declared).is_none() {
        return Err(AcceptanceError::UnsupportedDialect {
            gts_id: gts_id.to_owned(),
            found: declared.to_owned(),
        });
    }
    if let Some(path) = conflicting_dialect(content, "$", true) {
        return Err(AcceptanceError::ConflictingDialect {
            gts_id: gts_id.to_owned(),
            path,
        });
    }
    Ok(())
}

fn normalize_dialect(declared: &str) -> Option<&'static str> {
    DRAFT_07_SPELLINGS.contains(&declared).then_some(DRAFT_07)
}

/// The path of the first `$schema` below the root that does not normalize onto
/// the same dialect. A nested `$schema` equal to the root's — after
/// normalization, so `…/schema` and `…/schema#` agree — is not a conflict
/// (ADR-0014).
fn conflicting_dialect(value: &Value, path: &str, is_root: bool) -> Option<String> {
    match value {
        Value::Object(map) => {
            if !is_root
                && let Some(declared) = map.get("$schema").and_then(Value::as_str)
                && normalize_dialect(declared).is_none()
            {
                return Some(path.to_owned());
            }
            map.iter().find_map(|(key, child)| {
                conflicting_dialect(child, &format!("{path}.{key}"), false)
            })
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(i, child)| conflicting_dialect(child, &format!("{path}[{i}]"), false)),
        _ => None,
    }
}

/// Whether the candidate has a cross-minor compatibility check for `force` to
/// waive: a minor-bearing segment past `M.0`, at a stable major. Request-static,
/// which is why it belongs to acceptance — whether the waived comparison *would*
/// have failed stays a worker decision under the family lock.
fn has_cross_minor_check(id: &GtsId) -> bool {
    let Some(last) = id.segments().last() else {
        return false;
    };
    match (last.ver_major_opt(), last.ver_minor()) {
        (Some(0) | None, _) | (_, None | Some(0)) => false,
        (Some(_), Some(_)) => true,
    }
}

/// The keyed read both replay paths make.
///
/// Called twice: before the insert, and again by the loser of a concurrent
/// acceptance. The second call is deliberately **outside** the rolled-back
/// transaction — on `PostgreSQL` a constraint violation poisons it, so a re-read
/// inside would fail for a second, unrelated reason.
///
/// A plain transaction rather than [`snapshot_read`]: this is **one** statement, and
/// one statement is atomic on its own, so there is no snapshot to hold across
/// anything. It runs in a transaction at all only because the ports take `&DbTx`.
async fn find_operation_by_key(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<AcceptanceError>,
    scope: &AccessScope,
    validated: &Validated,
) -> Result<Option<OperationRow>, AcceptanceError> {
    let stores = Arc::clone(stores);
    let scope = scope.clone();
    let scope_hash = validated.idempotency_scope_hash;
    let key = validated.idempotency_key.clone();
    db.transaction(move |tx| {
        Box::pin(async move {
            Ok(stores
                .find_by_idempotency(tx, &scope, &scope_hash, &key)
                .await?)
        })
    })
    .await
}

#[cfg(test)]
#[path = "acceptance_tests.rs"]
mod acceptance_tests;
