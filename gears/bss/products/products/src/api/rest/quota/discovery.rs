//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-intake:p1
//! The discovery endpoint (P-D-268): read one complete snapshot of a target through its plugin
//! client, judge every record, and stage the snapshot with one candidate per valid record and its
//! notes, or answer the stored revision.
//!
//! The order of the checks: the caller, `author` on `quota_adapter_target` (anchored to the
//! caller's tenant), the target, the replay store (P-D-198), the plugin client (none registered:
//! 501 `QUOTA_DISCOVERY_CLOSED` before any read or write), the snapshot, then the stored revision,
//! which answers a replay or a conflict, and the target's highest revision, which refuses a stale
//! one, before any record is judged, so a registry outage never turns a replay or a stale revision
//! into a 503. Only then is every record judged with one memo, outside any
//! transaction (an outage: 503, nothing stored). One deadline bounds the read and the judging; the
//! staging is not cut. Then [`stage_snapshot`] stages: one transaction
//! claims the key, stages and records the receipt, and a staging that lost the insert race
//! answers the winner from a fresh transaction, which records the receipt of a keyed request
//! too.
use std::sync::Arc;
use std::time::{Duration, Instant};

use authz_resolver_sdk::PolicyEnforcer;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{Extension, Router};
use serde::Deserialize;
use serde_json::Value;
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit_db::secure::{AccessScope, DBRunner, TxConfig};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{
    ProductsQuotaDiscoveryResult, ProductsQuotaNote, ProductsQuotaRefusal, ProductsQuotaSnapshot,
};
use super::intake::{self, StageOutcome, StagedIntake};
use super::targets::{TAG, load_target, target_scope};
use super::{On, QuotaState, TARGETS, candidate_dtos, registry_unavailable};
use crate::api::rest::{ApiState, TxError, contention_db_err, replay, require_authenticated};
use crate::authz::actions;
use crate::domain::error::DomainError;
use crate::domain::quota::intake::{IntakePlan, RejectedRecord, plan_intake};
use crate::domain::quota::ports::{QuotaRegistry, ResolvedDiscovery};
use crate::domain::quota::record::{JudgeError, JudgedRecord, Resolutions, judge_record};
use crate::domain::quota::snapshot::{SnapshotError, VerifiedSnapshot, read_snapshot};
use crate::domain::quota::{QuotaTarget, StoredSnapshot};
use crate::infra::idempotency::IdempotencyClaimInput;
use crate::infra::storage::repo::quota_catalog_repo as catalog;
use crate::infra::storage::{RepoError, RepoRefusal, stored_now};

/// No discovery adapter is registered for the target's plugin instance.
pub const QUOTA_DISCOVERY_CLOSED: &str = "QUOTA_DISCOVERY_CLOSED";
/// The pages do not make one complete snapshot.
pub const QUOTA_DISCOVERY_INCOMPLETE: &str = "QUOTA_DISCOVERY_INCOMPLETE";
/// The adapter failed or did not answer in time.
pub const QUOTA_DISCOVERY_UNAVAILABLE: &str = "QUOTA_DISCOVERY_UNAVAILABLE";

/// What the discovery route's request timeout adds to the discovery deadline (P-D-268): the
/// staging transaction runs after the deadline's cut and is not cut itself.
pub const QUOTA_DISCOVERY_STAGING_ALLOWANCE: Duration = Duration::from_secs(15);

/// Register the discovery operation. Its request timeout is `deadline` plus
/// [`QUOTA_DISCOVERY_STAGING_ALLOWANCE`] (P-D-268), so a discovery past its deadline answers 503
/// `QUOTA_DISCOVERY_INCOMPLETE` before the API gateway cuts the request, whatever the gateway's
/// default timeout is.
pub(super) fn register(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    deadline: Duration,
) -> Router {
    OperationBuilder::post(format!("{TARGETS}/{{id}}/discoveries"))
        .operation_id("bss_products.run_quota_discovery")
        .summary("Discover a target's quota capabilities")
        .description(
            "Reads one complete capability snapshot of the target through its discovery plugin, \
             judges every record through types-registry, and stages the snapshot with one \
             candidate per valid record; the others are `rejected` notes, and approved \
             capabilities that changed or are gone are `drift` and `retired` notes (P-D-268). No \
             body. 201 when staged, 200 for the stored snapshot of the same revision and content. \
             Asks `author` on `quota_adapter_target`. Refusals: 400 VALIDATION for an \
             Idempotency-Key that is blank or too long; 404 for an unknown or foreign target; \
             409 QUOTA_SOURCE_CONFLICT when other content is stored at the revision, \
             QUOTA_SOURCE_STALE for a revision below the target's highest, IDEMPOTENCY_CONFLICT, \
             IDEMPOTENCY_KEY_IN_FLIGHT; 501 QUOTA_DISCOVERY_CLOSED when no discovery adapter is \
             registered for the plugin instance, before any read or write; 503 \
             QUOTA_DISCOVERY_INCOMPLETE, QUOTA_DISCOVERY_UNAVAILABLE or \
             QUOTA_REGISTRY_UNAVAILABLE, with nothing stored.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .request_timeout(deadline.saturating_add(QUOTA_DISCOVERY_STAGING_ALLOWANCE))
        .path_param("id", "The target's id")
        .param(replay::param())
        .handler(discover)
        .json_response_with_schema::<ProductsQuotaDiscoveryResult>(
            openapi,
            StatusCode::CREATED,
            "The staged snapshot, its candidates and its notes.",
        )
        .json_response_with_schema::<ProductsQuotaDiscoveryResult>(
            openapi,
            StatusCode::OK,
            "The stored snapshot of the same revision and content, its candidates and its notes.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .problem_response(openapi, StatusCode::NOT_IMPLEMENTED, "Not Implemented")
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// A snapshot that could not be read whole: 503, nothing stored.
fn snapshot_failure(error: SnapshotError) -> CanonicalError {
    let (code, detail) = match error {
        SnapshotError::Incomplete(why) => (QUOTA_DISCOVERY_INCOMPLETE, why),
        SnapshotError::Unavailable(why) => (QUOTA_DISCOVERY_UNAVAILABLE, why),
    };
    DomainError::QuotaUnavailable { code, detail }.into()
}

/// Every record judged with one memo, so the registry is asked once per entity; a refusal is a
/// rejected record, and an outage at any record is 503 for the whole intake.
async fn judge_all(
    registry: &dyn QuotaRegistry,
    ctx: &SecurityContext,
    items: &[Value],
) -> Result<Vec<Result<JudgedRecord, RejectedRecord>>, CanonicalError> {
    let mut memo = Resolutions::default();
    let mut judged = Vec::with_capacity(items.len());
    for (index, raw) in items.iter().enumerate() {
        judged.push(match judge_record(registry, ctx, raw, &mut memo).await {
            Ok(record) => Ok(record),
            Err(JudgeError::Refused(refusal)) => Err(RejectedRecord::of(index, raw, refusal)),
            Err(JudgeError::Unavailable(why)) => return Err(registry_unavailable(&why)),
        });
    }
    Ok(judged)
}

/// The judging of a snapshot's records outlasted what the discovery deadline left after the
/// read: 503 `QUOTA_DISCOVERY_INCOMPLETE`, nothing stored (P-D-268).
fn judged_too_late(target: Uuid, deadline: Duration) -> CanonicalError {
    tracing::warn!(
        target: "bss_products.quota.discovery",
        quota_target = %target,
        deadline_ms = deadline.as_millis(),
        "bss-products: a quota discovery's judging outlasted its deadline"
    );
    DomainError::QuotaUnavailable {
        code: QUOTA_DISCOVERY_INCOMPLETE,
        detail: "the records were not judged within the discovery deadline".to_owned(),
    }
    .into()
}

/// One stored note of `review_json`'s `rejected` list.
#[derive(Deserialize)]
struct StoredRefusal {
    record: String,
    code: String,
    field: String,
    detail: String,
}

/// One stored note of `review_json`'s `drift` or `retired` list.
#[derive(Deserialize)]
struct StoredNote {
    capability_key: String,
    mapping_id: Uuid,
    mapping_version: String,
}

/// The stored notes; a `review_json` that does not read is a corrupt row.
#[derive(Deserialize)]
struct StoredNotes {
    rejected: Vec<StoredRefusal>,
    drift: Vec<StoredNote>,
    retired: Vec<StoredNote>,
}

fn notes_of(snapshot: &StoredSnapshot) -> Result<StoredNotes, RepoError> {
    serde_json::from_value(snapshot.review_json.clone()).map_err(|e| {
        RepoError::CorruptRow(format!(
            "a stored quota row does not read: products_quota_snapshot, id {}: review_json: {e}",
            snapshot.id
        ))
    })
}

fn note(stored: StoredNote) -> ProductsQuotaNote {
    ProductsQuotaNote {
        capability_key: stored.capability_key,
        mapping_id: stored.mapping_id,
        mapping_version: stored.mapping_version,
    }
}

/// A staged or recovered intake as the endpoint answers it, from the stored rows only: 201 for a
/// first staging, 200 for a replay, and the same body for both.
async fn answer(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    outcome: StageOutcome,
) -> Result<(StatusCode, ProductsQuotaDiscoveryResult), TxError> {
    let (status, staged) = match outcome {
        StageOutcome::Created(staged) => (StatusCode::CREATED, staged),
        StageOutcome::Replayed(staged) => (StatusCode::OK, staged),
    };
    let StagedIntake {
        snapshot,
        candidates,
    } = staged;
    let notes = notes_of(&snapshot).map_err(TxError::Repo)?;
    let answered = candidate_dtos(runner, scope, tenant, candidates)
        .await
        .map_err(TxError::Repo)?;
    Ok((
        status,
        ProductsQuotaDiscoveryResult {
            snapshot: ProductsQuotaSnapshot {
                id: snapshot.id,
                snapshot_id: snapshot.snapshot_id,
                source_revision: snapshot.source_revision,
                observed_at: snapshot.observed_at,
                item_count: snapshot.item_count,
                content_digest: snapshot.content_digest,
            },
            candidates: answered,
            rejected: notes
                .rejected
                .into_iter()
                .map(|r| ProductsQuotaRefusal {
                    record: r.record,
                    code: r.code,
                    field: r.field,
                    detail: r.detail,
                })
                .collect(),
            drift: notes.drift.into_iter().map(note).collect(),
            retired: notes.retired.into_iter().map(note).collect(),
        },
    ))
}

/// The stored revision of `snapshot` as the answer (a replay, or a 409 conflict), a revision below
/// the target's highest stored as 409 stale, or `None` when the revision is new. A keyed request
/// records its answer in a transaction of its own, so a retry under its key answers it again.
async fn recovered(
    api: &ApiState,
    scope: &AccessScope,
    tenant: Uuid,
    target_id: Uuid,
    snapshot: &VerifiedSnapshot,
    claim: Option<&IdempotencyClaimInput>,
) -> Result<Option<Response>, CanonicalError> {
    let conn = On::TARGET.conn(api)?;
    let (revision, digest) = (&snapshot.source_revision, &snapshot.content_digest);
    let Some(outcome) = intake::settled(&conn, scope, tenant, target_id, revision, digest)
        .await
        .map_err(|e| On::TARGET.tx(e))?
    else {
        return Ok(None);
    };
    let Some(claim) = claim.cloned() else {
        // Built as a receipt is, so a replay answers the first answer's bytes.
        let (status, body) = answer(&conn, scope, tenant, outcome)
            .await
            .map_err(|e| On::TARGET.tx(e))?;
        return replay::finish(&conn, tenant, None, status, &body)
            .await
            .map(Some)
            .map_err(|e| On::TARGET.tx(e));
    };
    let scope_tx = scope.clone();
    api.db
        .db()
        .transaction_with_retry::<Response, TxError, _, _>(
            TxConfig::default(),
            contention_db_err,
            move |tx| {
                let (scope, claim, outcome) = (scope_tx.clone(), claim.clone(), outcome.clone());
                Box::pin(async move {
                    if let Some(response) = replay::begin(tx, tenant, Some(&claim)).await? {
                        return Ok(response);
                    }
                    let (status, body) = answer(tx, &scope, tenant, outcome).await?;
                    replay::finish(tx, tenant, Some(&claim), status, &body).await
                })
            },
        )
        .await
        .map(Some)
        .map_err(|e| On::TARGET.tx(e))
}

/// One discovery's staging (P-D-268), the path `discover` runs once every record is judged: one
/// transaction claims the key, stages the snapshot, its candidates and the audit row (or answers
/// the stored revision, or refuses a conflicting or stale one) and records the receipt; a staging
/// that lost the insert race answers the winner from a fresh transaction, which records the
/// receipt of a keyed request too. Nothing inside asks types-registry or the plugin. `rows` is the
/// tenant's scope (the target's grant, `tenant_only`). Public for the Postgres race suite only.
///
/// # Errors
/// 409 `QUOTA_SOURCE_CONFLICT`, `QUOTA_SOURCE_STALE`, `CONTENDED` (a lost race that did not
/// settle, or contention past the retries), `IDEMPOTENCY_CONFLICT`, `IDEMPOTENCY_KEY_IN_FLIGHT`;
/// storage failures; each on the adapter target.
#[doc(hidden)]
pub async fn stage_snapshot(
    api: &ApiState,
    rows: &AccessScope,
    (tenant, actor): (Uuid, Uuid),
    target: &QuotaTarget,
    (snapshot, plan): (&VerifiedSnapshot, &IntakePlan),
    claim: Option<IdempotencyClaimInput>,
) -> Result<Response, CanonicalError> {
    let (row, candidates) = intake::prepare(target, snapshot, plan, actor, stored_now())?;
    let (scope_tx, claim_tx) = (rows.clone(), claim.clone());
    let staged = api
        .db
        .db()
        .transaction_with_retry::<Response, TxError, _, _>(
            TxConfig::default(),
            contention_db_err,
            move |tx| {
                let (scope, claim) = (scope_tx.clone(), claim_tx.clone());
                let (row, candidates) = (row.clone(), candidates.clone());
                Box::pin(async move {
                    if let Some(response) = replay::begin(tx, tenant, claim.as_ref()).await? {
                        return Ok(response);
                    }
                    let outcome = intake::stage_in(tx, &scope, tenant, &row, &candidates).await?;
                    let (status, body) = answer(tx, &scope, tenant, outcome).await?;
                    replay::finish(tx, tenant, claim.as_ref(), status, &body).await
                })
            },
        )
        .await;
    match staged {
        // Postgres aborted the losing transaction: answer the winner from a fresh one.
        Err(TxError::Repo(RepoError::Refused(RepoRefusal::QuotaSnapshotTaken))) => {
            recovered(api, rows, tenant, target.id, snapshot, claim.as_ref())
                .await?
                .ok_or_else(intake::unsettled)
        }
        other => other.map_err(|e| On::TARGET.tx(e)),
    }
}

async fn discover(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-intake:p1:inst-quota-discover
    let tenant = ctx.subject_tenant_id();
    let actor = ctx.subject_id();
    let scope = target_scope(&enforcer, &ctx, actions::AUTHOR).await?;
    let conn = On::TARGET.conn(&state.api)?;
    let target = load_target(&conn, &scope, tenant, &id, On::TARGET).await?;
    let endpoint = format!("{TARGETS}/{}/discoveries", target.id);
    let claim = On::TARGET.input(&state.api, &headers, endpoint, &Value::Null)?;
    if let Some(response) = On::TARGET
        .replayed(&state.api, tenant, claim.as_ref())
        .await?
    {
        return Ok(response);
    }
    let client = match state.discovery.resolve(&target.plugin_instance_id) {
        ResolvedDiscovery::Client(client) => client,
        ResolvedDiscovery::Closed => {
            return Err(On::TARGET.refuse(DomainError::QuotaClosed {
                code: QUOTA_DISCOVERY_CLOSED,
                detail: "no discovery adapter is registered for the target's plugin instance"
                    .into(),
            }));
        }
    };
    // One deadline bounds the adapter read and the judging of its records (P-D-268); the route's
    // request timeout adds the staging allowance to it, and the staging transaction is not cut.
    let started = Instant::now();
    let snapshot = read_snapshot(client.as_ref(), &ctx, &target.target_key, state.settings)
        .await
        .map_err(snapshot_failure)?;
    // @cpt-end:cpt-cf-bss-products-flow-quota-catalog-intake:p1:inst-quota-discover
    // @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-intake:p1:inst-quota-stage
    // The target passed the full scope; its snapshot, candidates and audit rows are the tenant's.
    let rows = scope.tenant_only();
    if let Some(response) = recovered(
        &state.api,
        &rows,
        tenant,
        target.id,
        &snapshot,
        claim.as_ref(),
    )
    .await?
    {
        return Ok(response);
    }
    let deadline = state.settings.deadline;
    let left = deadline.saturating_sub(started.elapsed());
    let judged = tokio::time::timeout(
        left,
        judge_all(state.registry.as_ref(), &ctx, &snapshot.items),
    )
    .await
    .map_err(|_| judged_too_late(target.id, deadline))??;
    let approved = catalog::approved_mappings_of_target(&conn, &rows, tenant, target.id)
        .await
        .map_err(|e| On::TARGET.stored(&e))?;
    let plan = plan_intake(judged, &approved);
    stage_snapshot(
        &state.api,
        &rows,
        (tenant, actor),
        &target,
        (&snapshot, &plan),
        claim,
    )
    .await
    // @cpt-end:cpt-cf-bss-products-flow-quota-catalog-intake:p1:inst-quota-stage
}
