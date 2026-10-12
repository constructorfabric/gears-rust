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
//! | 3 registration policy | here (via [`RegistrationPolicy`]), for creations only |
//! | 4 managed identifier profile | here |
//! | 5 declared identity and dialect | here |
//! | 6 `force` | here |
//! | 7 ADR-0015 major-0 quarantine | **the worker** — see below |
//! | 8 canonicalize, fingerprint, idempotency | here |
//!
//! Step 7 runs in the worker over the dependency edges extracted by
//! [`unit::evaluate`](super::unit), keeping malformed references and quarantine
//! refusals in the admission-stage vocabulary (P16).
//!
use std::collections::{BTreeMap, BTreeSet, btree_map};
use std::sync::Arc;

use gts::{GTS_ID_URI_PREFIX, GtsId, GtsIdSegment};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, ScopeError};
use toolkit_db::{DBProvider, DbError};
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::fingerprint::{
    DeletionFingerprintTarget, FingerprintCandidate, FingerprintInput, P0_PRINCIPAL_ID,
    RequestFingerprint, RequestScope, ScopeHash, canonical_text, deletion_fingerprint,
    idempotency_scope_hash, request_fingerprint,
};
use super::{
    Accepted, DeleteRequest, DeleteTarget, OperationDispatch, Precondition, SubmitRequest,
};
use crate::config::TypesRegistryConfig;
use crate::domain::compat::{normalize_dialect, select_baseline};
use crate::domain::enums::{OperationKind, OwnershipScope, Plane};
use crate::domain::key::{EntityKey, MAX_KEY_LEN};
use crate::domain::policy::{PolicyRefusal, RegistrationPolicy};
use crate::domain::ports::metrics::{AdmissionMetrics, PassLabels, RefusalStage};
use crate::domain::ports::{
    NewOperation, NewOperationItem, OperationItemRow, OperationRow, Stores,
};

/// Largest `Idempotency-Key` the column accepts (`varchar(255)`).
pub(crate) const MAX_IDEMPOTENCY_KEY: usize = 255;

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
    /// The key is not carried: it is unbounded caller input, and would otherwise
    /// be echoed into the Problem and the refusal log.
    #[error("a key must be at most {MAX_KEY_LEN} bytes; this one is {length}")]
    KeyTooLong { length: usize },
    /// A registration's [`Self::KeyTooLong`]: same bound and reason for not
    /// carrying the value, but the field is `gts_id`.
    #[error("a GTS identifier must be at most {MAX_KEY_LEN} bytes; this one is {length}")]
    IdentifierTooLong { length: usize },
    #[error("'{gts_id}' appears twice in one batch")]
    DuplicateCandidate { gts_id: String },
    /// A deletion's duplicate: two keys naming one entity, possibly by different
    /// kinds of key, so the refusal names both positions rather than one string.
    #[error("items[{first_index}] and items[{second_index}] name the same entity")]
    DuplicateTarget {
        first_index: usize,
        second_index: usize,
    },
    #[error("{0}")]
    PolicyRefused(#[source] PolicyRefusalError),
    #[error("'{gts_id}' carries an explicit UUID tail, which is not registrable")]
    ExplicitUuidTail { gts_id: String },
    #[error("registered Instance '{gts_id}' must name a stable version: {reason}")]
    InstanceVersionProfile { gts_id: String, reason: String },
    #[error("Type Schema '{gts_id}' declares no string top-level $id")]
    MissingSchemaId { gts_id: String },
    /// The declared value is deliberately not carried: it is unbounded caller
    /// input, checked before the document size limit, and would otherwise be
    /// echoed into the Problem detail and the refusal log.
    #[error(
        "Type Schema '{gts_id}' declares a top-level $id other than '{GTS_ID_URI_PREFIX}{gts_id}'"
    )]
    SchemaIdMismatch { gts_id: String },
    #[error("'{gts_id}' declares no top-level $schema")]
    MissingDialect { gts_id: String },
    #[error("'{gts_id}' declares dialect '{found}', which is not the Draft-07 spelling set")]
    UnsupportedDialect { gts_id: String, found: String },
    #[error("'{gts_id}' declares a differing $schema at '{path}'")]
    ConflictingDialect { gts_id: String, path: String },
    #[error("'{gts_id}' carries no document, which a registration requires")]
    MissingContent { gts_id: String },
    /// Never "delete if present": the only other reading of an absent version is
    /// a deletion that races whoever last wrote the entity.
    #[error("deleting '{gts_id}' requires a positive expected_resource_version")]
    DeletionRequiresVersion { gts_id: String },
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
    /// The last segment has no readable major, so baseline selection fails.
    #[error("'{gts_id}' names no readable major, so no compatibility baseline exists")]
    UnreadableVersion { gts_id: String },
    #[error("minor-bearing Type Schema '{gts_id}' is content-immutable")]
    MinorTypeSchemaRevision { gts_id: String },
    #[error(
        "expected_resource_version 0 on '{gts_id}' is refused: omit the field to require absence"
    )]
    ZeroPrecondition { gts_id: String },
    /// A deletion's [`Self::ZeroPrecondition`]: omitting the version is refused
    /// too, so the registration advice would be wrong here.
    #[error(
        "expected_resource_version 0 on '{gts_id}' is refused: deleting requires the positive \
         version being deleted"
    )]
    DeletionZeroPrecondition { gts_id: String },
    #[error("expected_resource_version {version} on '{gts_id}' is negative")]
    NegativePrecondition { gts_id: String, version: i64 },
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

impl AcceptanceError {
    /// The stable machine reason this refusal is counted and logged under.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::MissingIdempotencyKey => "missing_idempotency_key",
            Self::IdempotencyKeyTooLong { .. } => "idempotency_key_too_long",
            Self::EmptyBatch => "empty_batch",
            Self::BatchTooLarge { .. } => "batch_too_large",
            Self::InvalidIdentifier { .. } => "invalid_identifier",
            Self::KeyTooLong { .. } => "key_too_long",
            Self::IdentifierTooLong { .. } => "identifier_too_long",
            Self::DuplicateCandidate { .. } | Self::DuplicateTarget { .. } => "duplicate_candidate",
            Self::PolicyRefused(_) => "policy_refused",
            Self::ExplicitUuidTail { .. } => "explicit_uuid_tail",
            Self::InstanceVersionProfile { .. } => "instance_version_profile",
            Self::MissingSchemaId { .. } => "missing_schema_id",
            Self::SchemaIdMismatch { .. } => "schema_id_mismatch",
            Self::MissingDialect { .. } => "missing_dialect",
            Self::UnsupportedDialect { .. } => "unsupported_dialect",
            Self::ConflictingDialect { .. } => "conflicting_dialect",
            Self::MissingContent { .. } => "missing_content",
            Self::DeletionRequiresVersion { .. } => "deletion_requires_version",
            Self::AuthoredDocumentTooLarge { .. } => "authored_document_too_large",
            Self::ForceNotPermitted { .. } => "force_not_permitted",
            Self::ForceHasNothingToWaive { .. } => "force_has_nothing_to_waive",
            Self::UnreadableVersion { .. } => "unreadable_version",
            Self::MinorTypeSchemaRevision { .. } => "minor_type_schema_revision",
            Self::ZeroPrecondition { .. } | Self::DeletionZeroPrecondition { .. } => {
                "zero_precondition"
            }
            Self::NegativePrecondition { .. } => "negative_precondition",
            Self::FingerprintConflict { .. } => "fingerprint_conflict",
            Self::Dispatch(_) => "dispatch_failure",
            Self::Storage(_) => "storage_failure",
            Self::Db(_) => "database_failure",
        }
    }
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
    /// Admission metrics.
    pub metrics: &'a Arc<dyn AdmissionMetrics>,
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
    let key = validate_envelope(
        ctx,
        request.idempotency_key.as_deref(),
        request.candidates.len(),
    )?;
    let limit = ctx.config.limits.batch_candidates;

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut items = Vec::with_capacity(request.candidates.len());
    let mut fingerprint_forces = Vec::with_capacity(request.candidates.len());
    let mut fingerprint_ids: Vec<String> = Vec::with_capacity(request.candidates.len());

    for (index, candidate) in request.candidates.iter().enumerate() {
        // --- step 2: candidate identifiers -------------------------------
        // Before a refusal copies it into the Problem and the refusal log.
        if candidate.gts_id.len() > MAX_KEY_LEN {
            return Err(AcceptanceError::IdentifierTooLong {
                length: candidate.gts_id.len(),
            });
        }
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
            // The claim is not taken on trust: the worker commits it through
            // `commit_revision`, which refuses an absent identifier, so naming a
            // version cannot register a new entity.
            Some(v) => Precondition::Version(v),
        };
        // ADR-0004: a minor-bearing Type Schema is content-immutable. During
        // ceiling C9 this permanent refusal also bounds the implementation window.
        if matches!(expected, Precondition::Version(_)) && is_minor_bearing_type_schema(&id) {
            return Err(AcceptanceError::MinorTypeSchemaRevision {
                gts_id: id.id().to_owned(),
            });
        }

        // --- step 3: registration policy ---------------------------------
        // Creations only (SPEC §8.1 step 3, DESIGN §3.2). Revision and deletion
        // require an existing entity downstream, so neither can bypass the allowlist
        // by creating one.
        //
        // ponytail: ceiling C6 — neither path checks owner/principal authority in P0.
        // See `unit::commit_revision` and `deletion::commit_deletion`.
        if expected == Precondition::MustNotExist {
            ctx.policy
                .admits(&id, OwnershipScope::Global)
                .map_err(|refusal| AcceptanceError::PolicyRefused(PolicyRefusalError(refusal)))?;
        }

        // --- step 4: managed identifier profile --------------------------
        check_identifier_profile(&id)?;

        // --- step 5: declared identity and dialect -----------------------
        let content =
            candidate
                .content
                .as_ref()
                .ok_or_else(|| AcceptanceError::MissingContent {
                    gts_id: id.id().to_owned(),
                })?;
        if id.is_type() {
            check_schema_id(id.id(), content)?;
            check_dialect(id.id(), content)?;
        }

        // --- step 6: force ------------------------------------------------
        // Check the deployment flag and baseline eligibility here; the worker
        // evaluates compatibility and re-authorizes the waiver.
        if candidate.force {
            if !ctx.config.allow_compatibility_force {
                return Err(AcceptanceError::ForceNotPermitted {
                    gts_id: id.id().to_owned(),
                });
            }
            // Use baseline selection so acceptance and evaluation agree on waiver eligibility.
            match select_baseline(&id, expected) {
                Ok(baseline) if baseline.waivable() => {}
                Ok(_) => {
                    return Err(AcceptanceError::ForceHasNothingToWaive {
                        gts_id: id.id().to_owned(),
                    });
                }
                // No baseline exists to waive, which is not the same refusal as a
                // baseline that nothing may waive.
                Err(unreadable) => {
                    return Err(AcceptanceError::UnreadableVersion {
                        gts_id: unreadable.gts_id,
                    });
                }
            }
        }

        // Step 7 runs in the worker over the extracted dependency edges.

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
        fingerprint_ids.push(id.id().to_owned());
        items.push(NewOperationItem {
            item_no,
            key: EntityKey::GtsId(id.id().to_owned()),
            precondition: expected,
            // The wire and ADR-0004 say `force`; the column says `compat_forced`.
            // This is the one place the two names meet.
            compat_forced: candidate.force,
            request_payload: canonical,
        });
    }

    let fingerprint_candidates: Vec<FingerprintCandidate<'_>> = items
        .iter()
        .zip(&fingerprint_forces)
        .zip(&fingerprint_ids)
        .map(|((item, force), gts_id)| FingerprintCandidate {
            gts_id,
            canonical_body: &item.request_payload,
            precondition: item.precondition,
            force: *force,
        })
        .collect();

    Ok(Validated {
        kind: OperationKind::Registration,
        dry_run: request.dry_run,
        // Past the check above, so a plain `String`: this key exists.
        idempotency_key: key.to_owned(),
        idempotency_scope_hash: p0_scope_hash(),
        request_fingerprint: request_fingerprint(&FingerprintInput {
            kind: OperationKind::Registration,
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

/// Step 1: the `Idempotency-Key` and the batch size.
fn validate_envelope<'a>(
    ctx: &AcceptanceContext<'_>,
    idempotency_key: Option<&'a str>,
    count: usize,
) -> Result<&'a str, AcceptanceError> {
    // An absent header and a blank one are one refusal: both leave acceptance
    // without the key a replay would have to match.
    let key = idempotency_key.unwrap_or_default().trim();
    if key.is_empty() {
        return Err(AcceptanceError::MissingIdempotencyKey);
    }
    if key.len() > MAX_IDEMPOTENCY_KEY {
        return Err(AcceptanceError::IdempotencyKeyTooLong { length: key.len() });
    }
    if count == 0 {
        return Err(AcceptanceError::EmptyBatch);
    }
    let limit = ctx.config.limits.batch_candidates;
    if count > limit {
        return Err(AcceptanceError::BatchTooLarge { count, limit });
    }
    Ok(key)
}

/// Validate a deletion. It reads no entity state, so a Registry Reference stays
/// unresolved here and the worker decides what it names.
///
/// # Errors
/// One [`AcceptanceError`] per refusal reason.
pub fn validate_deletion(
    ctx: &AcceptanceContext<'_>,
    request: &DeleteRequest,
) -> Result<Validated, AcceptanceError> {
    let key = validate_envelope(
        ctx,
        request.idempotency_key.as_deref(),
        request.targets.len(),
    )?;
    validate_targets(ctx, key, request.dry_run, &request.targets)
}

/// Deletion targets are identified by Registry Reference: an identifier
/// determines it, so two spellings of one entity are one duplicate and one
/// fingerprint, whether or not the entity exists.
fn validate_targets(
    ctx: &AcceptanceContext<'_>,
    idempotency_key: &str,
    dry_run: bool,
    targets: &[DeleteTarget],
) -> Result<Validated, AcceptanceError> {
    // Each Registry Reference with the index of the target that first named it.
    let mut seen: BTreeMap<Uuid, usize> = BTreeMap::new();
    let mut items = Vec::with_capacity(targets.len());
    let mut fingerprinted = Vec::with_capacity(targets.len());
    for (index, target) in targets.iter().enumerate() {
        // Before `spelled` or a refusal copies the key. REST checks the raw string
        // too; this guards every other caller.
        if let EntityKey::GtsId(raw) = &target.key
            && raw.len() > MAX_KEY_LEN
        {
            return Err(AcceptanceError::KeyTooLong { length: raw.len() });
        }
        let spelled = target.key.to_string();
        let gts_uuid = match &target.key {
            EntityKey::GtsId(raw) => {
                let id = GtsId::try_new(raw).map_err(|e| AcceptanceError::InvalidIdentifier {
                    gts_id: raw.clone(),
                    reason: e.to_string(),
                })?;
                if id.id() != raw {
                    return Err(AcceptanceError::InvalidIdentifier {
                        gts_id: raw.clone(),
                        reason: format!("canonical form is '{}'", id.id()),
                    });
                }
                check_identifier_profile(&id)?;
                id.to_uuid()
            }
            EntityKey::Uuid(gts_uuid) => *gts_uuid,
        };
        match seen.entry(gts_uuid) {
            btree_map::Entry::Occupied(first) => {
                return Err(AcceptanceError::DuplicateTarget {
                    first_index: *first.get(),
                    second_index: index,
                });
            }
            btree_map::Entry::Vacant(slot) => {
                slot.insert(index);
            }
        }
        let precondition = match target.expected_resource_version {
            // Never "delete if present": the only other reading of an absent
            // version is a deletion that races whoever last wrote the entity.
            None => return Err(AcceptanceError::DeletionRequiresVersion { gts_id: spelled }),
            Some(0) => return Err(AcceptanceError::DeletionZeroPrecondition { gts_id: spelled }),
            Some(version) if version < 0 => {
                return Err(AcceptanceError::NegativePrecondition {
                    gts_id: spelled,
                    version,
                });
            }
            Some(version) => Precondition::Version(version),
        };
        let item_no = i32::try_from(index).map_err(|_| AcceptanceError::BatchTooLarge {
            count: targets.len(),
            limit: ctx.config.limits.batch_candidates,
        })?;
        fingerprinted.push(DeletionFingerprintTarget {
            gts_uuid,
            precondition,
        });
        items.push(NewOperationItem {
            item_no,
            key: target.key.clone(),
            precondition,
            compat_forced: false,
            // `ck_tr_operation_item_state` requires a non-null pending payload.
            request_payload: canonical_text(&Value::Null),
        });
    }
    Ok(Validated {
        kind: OperationKind::Deletion,
        dry_run,
        idempotency_key: idempotency_key.to_owned(),
        idempotency_scope_hash: p0_scope_hash(),
        request_fingerprint: deletion_fingerprint(
            p0_request_scope(OperationKind::Deletion, dry_run),
            &fingerprinted,
        ),
        items,
    })
}

/// Step 4: no explicit UUID tail, and a registered Instance names a stable
/// major without a minor (ADR-0004, ADR-0015).
fn check_identifier_profile(id: &GtsId) -> Result<(), AcceptanceError> {
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
    Ok(())
}

/// ponytail: ceiling C2 — the three inputs are constants in P0, so the key
/// namespace is global. See `fingerprint::P0_PRINCIPAL_ID`.
fn p0_scope_hash() -> ScopeHash {
    idempotency_scope_hash(Plane::Platform, None, P0_PRINCIPAL_ID)
}

const fn p0_request_scope(kind: OperationKind, dry_run: bool) -> RequestScope {
    RequestScope {
        kind,
        dry_run,
        plane: Plane::Platform,
        tenant_id: None,
        principal_id: P0_PRINCIPAL_ID,
        ownership_scope: OwnershipScope::Global,
    }
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
    let accepted = match validate(ctx, request) {
        Ok(validated) => accept_inner(stores, db, scope, dispatch, validated, now).await,
        Err(error) => Err(error),
    };
    count_refusal(
        ctx,
        OperationKind::Registration,
        request.dry_run,
        request.candidates.len(),
        &accepted,
    );
    accepted
}

/// [`accept`] for a deletion, whose keys may be Registry References.
///
/// # Errors
/// As [`accept`].
pub async fn accept_deletion(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<AcceptanceError>,
    scope: &AccessScope,
    ctx: &AcceptanceContext<'_>,
    dispatch: &Arc<dyn OperationDispatch>,
    request: &DeleteRequest,
    now: OffsetDateTime,
) -> Result<Accepted, AcceptanceError> {
    let accepted = match validate_deletion(ctx, request) {
        Ok(validated) => accept_inner(stores, db, scope, dispatch, validated, now).await,
        Err(error) => Err(error),
    };
    count_refusal(
        ctx,
        OperationKind::Deletion,
        request.dry_run,
        request.targets.len(),
        &accepted,
    );
    accepted
}

/// Count at the shared exit so every refusal is covered.
fn count_refusal(
    ctx: &AcceptanceContext<'_>,
    kind: OperationKind,
    dry_run: bool,
    candidates: usize,
    accepted: &Result<Accepted, AcceptanceError>,
) {
    let Err(error) = accepted else {
        return;
    };
    let reason = error.reason();
    // The request's own kind and mode: a synchronous refusal has no stored item
    // to read them from, and both are top-level fields it always carries.
    ctx.metrics.refused(
        RefusalStage::Acceptance,
        reason,
        PassLabels::new(kind, dry_run),
    );
    // The `warn` is for client refusals only.
    let infrastructure = matches!(
        error,
        AcceptanceError::Storage(_) | AcceptanceError::Db(_) | AcceptanceError::Dispatch(_)
    );
    if !infrastructure {
        tracing::warn!(
            reason,
            candidates,
            %error,
            "types_registry refused a submission"
        );
    }
}

/// Resolve the `Idempotency-Key` and commit a validated request.
async fn accept_inner(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<AcceptanceError>,
    scope: &AccessScope,
    dispatch: &Arc<dyn OperationDispatch>,
    validated: Validated,
    now: OffsetDateTime,
) -> Result<Accepted, AcceptanceError> {
    let validated = Arc::new(validated);

    // Replay before insert: the common case for a retrying client, and one read
    // against `operation` rather than a failed insert.
    if let Some(existing) = find_operation_by_key(stores, db, scope, &validated).await? {
        return resolve_replay(stores, db, scope, &existing, &validated).await;
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
                // Enqueue last, so any earlier failure rolls back before a wake
                // exists: an escaped wake means the rows are about to commit.
                let wake = tx_dispatch
                    .enqueue(tx, parent.id)
                    .await
                    .map_err(|e| AcceptanceError::Dispatch(e.into()))?;
                Ok((
                    Accepted {
                        operation_id: parent.id,
                        replayed: false,
                        status: parent.status,
                    },
                    wake,
                ))
            })
        })
        .await;

    match insert {
        Ok((accepted, wake)) => {
            // The rows are durable now; wake the sequencer against them.
            wake.fire();
            Ok(accepted)
        }
        // The unique constraint on (idempotency_scope_hash, idempotency_key) is the
        // serialization point between two concurrent acceptances — this layer has no
        // row to lock, and the read above cannot close the window. The loser re-reads
        // the winner outside the rolled-back transaction; see `load_replay`.
        Err(AcceptanceError::Storage(e)) if e.is_unique_violation() => {
            // The transaction rolled back before `enqueue`, so no wake exists to drop.
            let winner = find_operation_by_key(stores, db, scope, &validated)
                .await?
                .ok_or(AcceptanceError::Storage(ScopeError::Invalid(
                    "operation vanished between insert and re-read",
                )))?;
            resolve_replay(stores, db, scope, &winner, &validated).await
        }
        Err(e) => Err(e),
    }
}

/// A stored operation under this key: a replay when the fingerprint matches, a
/// `409` when it does not.
async fn resolve_replay(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<AcceptanceError>,
    scope: &AccessScope,
    existing: &OperationRow,
    validated: &Validated,
) -> Result<Accepted, AcceptanceError> {
    let matches = existing.request_fingerprint == validated.request_fingerprint
        || matches_legacy_deletion(stores, db, scope, existing, validated).await?;
    if matches {
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

/// Whether a deletion accepted before Registry-Reference fingerprints is this
/// request. Those resolved every UUID at acceptance and digested the identifier,
/// so recompute that digest, taking a UUID target's identifier from the stored
/// item at its position only when the two name the same Registry Reference.
async fn matches_legacy_deletion(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<AcceptanceError>,
    scope: &AccessScope,
    existing: &OperationRow,
    validated: &Validated,
) -> Result<bool, AcceptanceError> {
    // Before the read: neither a registration nor a registration's key can match.
    if validated.kind != OperationKind::Deletion || existing.kind != OperationKind::Deletion {
        return Ok(false);
    }
    let stores = Arc::clone(stores);
    let scope = scope.clone();
    let operation_id = existing.id;
    let recorded = db
        .transaction(move |tx| {
            Box::pin(async move { Ok(stores.find_items(tx, &scope, operation_id).await?) })
        })
        .await?;
    Ok(legacy_deletion_fingerprint(validated, &recorded) == Some(existing.request_fingerprint))
}

/// The digest the previous acceptance would have given `validated`, reading a
/// UUID target's identifier from `recorded` at its position; `None` when the two
/// cannot be the same request. Only a deletion has one: a registration never
/// takes this fallback, whatever its items.
pub(super) fn legacy_deletion_fingerprint(
    validated: &Validated,
    recorded: &[OperationItemRow],
) -> Option<RequestFingerprint> {
    if validated.kind != OperationKind::Deletion || recorded.len() != validated.items.len() {
        return None;
    }
    let mut gts_ids = Vec::with_capacity(recorded.len());
    for (item, recorded) in validated.items.iter().zip(recorded) {
        let gts_id = match (&item.key, &recorded.key) {
            (EntityKey::GtsId(gts_id), _) => gts_id.as_str(),
            (EntityKey::Uuid(gts_uuid), resolved @ EntityKey::GtsId(gts_id))
                if resolved.gts_uuid() == Some(*gts_uuid) =>
            {
                gts_id.as_str()
            }
            _ => return None,
        };
        gts_ids.push(gts_id);
    }
    let candidates: Vec<FingerprintCandidate<'_>> = validated
        .items
        .iter()
        .zip(gts_ids)
        .map(|(item, gts_id)| FingerprintCandidate {
            gts_id,
            canonical_body: &item.request_payload,
            precondition: item.precondition,
            force: false,
        })
        .collect();
    let scope = p0_request_scope(OperationKind::Deletion, validated.dry_run);
    Some(request_fingerprint(&FingerprintInput {
        kind: scope.kind,
        dry_run: scope.dry_run,
        plane: scope.plane,
        tenant_id: scope.tenant_id,
        principal_id: scope.principal_id,
        ownership_scope: scope.ownership_scope,
        candidates: &candidates,
    }))
}

/// Step 5. The document names the entity the item names: a Type Schema's
/// top-level `$id` is exactly `gts://<gts_id>`.
///
/// `gts_id` is already canonical (step 2), so exact string equality is the
/// canonical comparison. Nothing is trimmed or normalized: like step 2, a second
/// spelling of the same identity is refused as ambiguous rather than repaired,
/// and a bare `gts.` spelling is not a schema URI (GTS forbids it in `$id`).
/// Instances are not checked: their identity lives in the item alone.
fn check_schema_id(gts_id: &str, content: &Value) -> Result<(), AcceptanceError> {
    let declared = content.get("$id").and_then(Value::as_str).ok_or_else(|| {
        AcceptanceError::MissingSchemaId {
            gts_id: gts_id.to_owned(),
        }
    })?;
    if declared.strip_prefix(GTS_ID_URI_PREFIX) != Some(gts_id) {
        return Err(AcceptanceError::SchemaIdMismatch {
            gts_id: gts_id.to_owned(),
        });
    }
    Ok(())
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
    if let Some(path) = conflicting_dialect(content) {
        return Err(AcceptanceError::ConflictingDialect {
            gts_id: gts_id.to_owned(),
            path,
        });
    }
    Ok(())
}

/// The path of the first `$schema` below the root that does not normalize onto
/// the same dialect. A nested `$schema` equal to the root's — after
/// normalization, so `…/schema` and `…/schema#` agree — is not a conflict
/// (ADR-0014).
///
/// The root's own `$schema` belongs to [`check_dialect`], which has already
/// judged it, so this walk starts one level down rather than threading a
/// "you are the exempt node" flag through every frame of the recursion.
fn conflicting_dialect(root: &Value) -> Option<String> {
    conflicting_dialect_below(root, "$")
}

/// The same search over `value`'s children only.
fn conflicting_dialect_below(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::Object(map) => map
            .iter()
            .find_map(|(key, child)| conflicting_dialect_at(child, &format!("{path}.{key}"))),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(i, child)| conflicting_dialect_at(child, &format!("{path}[{i}]"))),
        _ => None,
    }
}

/// One node below the root: its own `$schema` first, then its children's.
fn conflicting_dialect_at(value: &Value, path: &str) -> Option<String> {
    if let Value::Object(map) = value
        && let Some(declared) = map.get("$schema")
    {
        let supported = declared
            .as_str()
            .is_some_and(|declared| normalize_dialect(declared).is_some());
        if !supported {
            return Some(path.to_owned());
        }
    }
    conflicting_dialect_below(value, path)
}

/// ADR-0004 makes a minor-bearing Type Schema an immutable published contract.
/// Its next minor is a new logical entity; only major-only Type Schemas and
/// Instances have a content-revision path.
fn is_minor_bearing_type_schema(id: &GtsId) -> bool {
    id.is_type()
        && id
            .segments()
            .last()
            .and_then(GtsIdSegment::ver_minor)
            .is_some()
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
