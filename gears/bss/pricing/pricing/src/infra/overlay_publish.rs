//! The overlay plane's publish commit — D-234's option (b).
//!
//! `inst-pl-commit` promises an approved overlay becomes *"a publish unit through
//! the Foundation engine (D-06)"*. It cannot be one of that engine's units:
//! [`PlanPublishUnit`](crate::domain::publish::PlanPublishUnit) is plan-shaped in
//! every field — a plan id, one of its revisions, a kind discriminator — and both
//! its constructors name a plan fact. An overlay has no plan, so the second half
//! of `inst-pl-commit` is a **new pipeline** rather than a third constructor, and
//! this module is it.
//!
//! Riding the targeted plans' units was the other candidate and is wrong on the
//! design set's own terms: an overlay's lines may target several plans or none,
//! so there is no unit to ride — and D-112 added the tenant `overlay_index`
//! subject precisely because per-plan resolution gave overlay *enumeration* no
//! access path.
//!
//! It lives in `infra` rather than in `domain` because it holds an `AccessScope`
//! and repositories, which dylint DE0301 forbids inside `src/domain/**` — the
//! same reason [`crate::infra::publish`] and [`crate::infra::read_model`] are
//! here. It is a **separate module** from `infra::publish` rather than a second
//! service inside it, because that module's every line is about `PlanShape`.
//!
//! # What one act writes
//!
//! One transaction: the revision flip and its predecessor's supersession (which
//! `overlay_repo` makes atomic because §6 requires it), one
//! `pricing_catalog_version_ref` row **per subject** against one registry handle,
//! and — through the flip — the audit record. D-112 makes that two subjects, and
//! D-133 makes it three when a revision moves the scope value.
//!
//! **One handle, not one per subject**, and the reason is a pin: the registry
//! batches (D-47), so two handles of one act may resolve to two versions, and a
//! pin landing between them would resolve the overlay document as live while the
//! index shard at or below it still did not list it. `pricing_catalog_version_ref`'s
//! key carries the subject beside the handle, so one handle covers them both.
//!
//! # The announcement, and why it was absent
//!
//! `PriceOverlayPublished` (D-248) is emitted at step 6, aggregated on the **overlay
//! id**. Until then this module carried a paragraph headed *"No outbox event"*,
//! arguing that the frozen `CatalogEvent` set named nothing overlay-shaped and that
//! **a wire event is the design set's to declare** (D-218's posture, one plane over),
//! so minting one here would be inventing surface — the same discipline as inventing
//! a wire code. That was right, and it is why the name arrived as a decision
//! extending `fr-event-contract` to fourteen rather than as a literal in this file.
//! The set turned out to be frozen in the **schema** as well, so it also cost
//! `chk_pricing_outbox_event_name`.
//!
//! # Both halves of §4.2 are enforced, and the second arrived late
//!
//! The **content** pin keeps a reviewer's decision honest: what commits is what
//! was approved. The **second validation run** keeps the *world* honest: what
//! commits is still admissible against a state that has moved since the submit.
//!
//! The second was owed for one wave, recorded here and on D-234 as blocked
//! because the rule set's world came from `overlay_repo::world_for`, "which takes
//! a provider rather than a runner". That was true of the wrapper and of nothing
//! else — every fact reader underneath it had always taken `&impl DBRunner` — so
//! the fix was to split out `world_on` and call it on the transaction. The
//! premise was narrower than the entry recording it, which is why it is stated
//! here rather than quietly closed.

use std::sync::Arc;

use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::approval::content_pin::overlay_content_hash;
use crate::domain::audit::AuditStamp;
use crate::domain::error::DomainError;
use crate::domain::overlay::OverlayLifecycle;
use crate::domain::overlay_rules::{OverlayCandidate, PRECEDENCE_DUPLICATE, conflict_of, validate};
use crate::domain::ports::CatalogVersionRegistryV1;
use crate::domain::publish::{OverlayPublishUnit, PublishAuthorization};
use crate::domain::read_model::{OverlayIndexShard, SubjectRef};
use crate::infra::registry_deadline::request_version_now;
use crate::infra::storage::repo::outbox_repo::PriceOverlayPublishedPayload;
use crate::infra::storage::repo::{
    NewOutboxEvent, PendingVersionRow, audit_repo, catalog_version_ref_repo, outbox_repo,
};
use crate::infra::storage::repo::{approval_repo, overlay_repo};
use crate::infra::storage::repo_failure;

/// What an overlay publish produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayPublishReceipt {
    /// The overlay that was published.
    pub price_overlay_id: Uuid,
    /// The revision that became current.
    pub revision: u64,
    /// The registry's **pending** handle. Not a `CatalogVersion`: the commit
    /// requests an assignment and `CatalogVersionPublished` resolves it.
    pub pending_ref: String,
    /// Every subject this act recorded a ref for — the document's, and the
    /// index shard or shards it rewrites.
    pub subjects: Vec<SubjectRef>,
}

/// The overlay publish engine.
#[derive(Clone)]
pub struct OverlayPublishService {
    db: DBProvider<DbError>,
    /// The sole incrementer of `CatalogVersion` for this plane, resolved at init.
    registry: Arc<dyn CatalogVersionRegistryV1>,
}

impl OverlayPublishService {
    /// Build over one database provider and the registry port.
    #[must_use]
    pub fn new(db: DBProvider<DbError>, registry: Arc<dyn CatalogVersionRegistryV1>) -> Self {
        Self { db, registry }
    }

    /// Publish an approved overlay revision. One transaction, or none of it.
    ///
    /// # The sequence, and why each move sits where it does
    ///
    /// 1. **Re-read the revision and check the pin, inside the transaction.**
    ///    `inst-ap-pin` scopes the TOCTOU void to a `submitted` record, so an
    ///    approved unit is *not* voided when its subject moves and the
    ///    approve-to-commit window is real. A check made before this transaction
    ///    opened would be a check the world can move behind.
    /// 2. **Resolve the shards.** The revision being published names one; a
    ///    predecessor whose scope value differs names a second, which is D-133's
    ///    *"two when a revision moves the scope value"*. Resolved **before** the
    ///    flip, because afterwards the predecessor reads `superseded` and the
    ///    shard it is leaving would have to be re-derived from a state that has
    ///    already moved.
    /// 3. **Request the `CatalogVersion`, fail closed**, after the refusals and
    ///    before the writes — [`crate::infra::publish`]'s ordering and its
    ///    reason: earlier, a refused commit leaves a pending handle nothing ever
    ///    commits, tripping `commit_overdue` for a publish that never happened.
    ///    The id is derived from `(tenant, overlay, revision)` so a rolled-back
    ///    retry re-requests the **same** handle instead of orphaning one.
    /// 4. **Flip the revision and supersede its predecessor**, which
    ///    `overlay_repo` does atomically because §6 requires it, and which writes
    ///    the audit record for the pair.
    /// 5. **Record one ref per subject**, all against the one handle.
    /// 6. **Announce the publish** (`PriceOverlayPublished`, D-248), aggregated on
    ///    the overlay rather than on a plan: an overlay is tenant-scoped and may
    ///    target no plan at all, so there is no plan stream to order it within.
    /// 7. **Void the units this publish did not consume** — a unit still
    ///    `submitted` over the revision just frozen is an orphan whose approval
    ///    could never lead to a publish.
    ///
    /// Step **1b** is §4.2's second validation run, enforced on the transaction
    /// runner. Without it a world change between the submit and the commit — a target
    /// plan retired, a scope value withdrawn from its taxonomy — goes uncaught,
    /// leaving only the submit's own run and a window of D-47's batching delay plus
    /// the reviewer's thinking time.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when the revision is absent;
    /// [`DomainError::LifecycleForbidden`] when it is not an open draft;
    /// [`DomainError::ApprovalContentMismatch`] when the content is not what was
    /// approved; [`DomainError::CatalogVersionUnavailable`] when the registry
    /// cannot be reached; [`DomainError::Internal`] on a storage failure.
    pub async fn commit(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant_id: Uuid,
        unit: OverlayPublishUnit,
        authorization: PublishAuthorization,
        stamp: AuditStamp,
    ) -> Result<OverlayPublishReceipt, DomainError> {
        let ctx = ctx.clone();
        let scope = scope.clone();
        let registry = Arc::clone(&self.registry);
        let OverlayPublishUnit {
            price_overlay_id,
            revision,
        } = unit;
        let request_id = publish_request_id(tenant_id, price_overlay_id, revision);

        let (_, outcome) = self
            .db
            .db()
            .in_transaction::<OverlayPublishReceipt, DomainError, _>(move |txn| {
                Box::pin(async move {
                    let record =
                        overlay_repo::load_on(txn, &scope, tenant_id, price_overlay_id, revision)
                            .await
                            .map_err(|e| repo_failure(&e))?
                            .ok_or_else(|| DomainError::NotFound {
                                subject: "price overlay revision".to_owned(),
                                id: format!("{price_overlay_id}/{revision}"),
                            })?;
                    if record.lifecycle_state != OverlayLifecycle::Draft {
                        return Err(DomainError::LifecycleForbidden(format!(
                            "price overlay {price_overlay_id} revision {revision} is {}; only an \
                             open draft revision publishes",
                            record.lifecycle_state.as_str()
                        )));
                    }

                    // 1. The approve->commit window, closed here and not at the
                    // surface.
                    if let Some(pinned) = authorization.pinned_content_hash()
                        && overlay_content_hash(&record.content()) != pinned
                    {
                        return Err(DomainError::ApprovalContentMismatch(format!(
                            "price overlay {price_overlay_id}/{revision}: the content approved \
                             under {} is not the content this commit would freeze; re-submit and \
                             have the change reviewed again",
                            authorization
                                .approval_ref()
                                .map_or_else(|| "-".to_owned(), |id| id.to_string()),
                        )));
                    }

                    // 1b. §4.2's **second validation run** (D-234 residue (1)).
                    // The pin above proves the *content* is what a reviewer
                    // approved; this proves the *world* still admits it. A target
                    // plan retired, a scope value withdrawn from its taxonomy or
                    // a precedence taken by someone else's publish between the
                    // submit and the commit all land here, and each of them would
                    // otherwise be frozen into a `CatalogVersion` by a commit
                    // whose only check was against a reviewer's snapshot.
                    //
                    // **On the transaction runner**, which is the whole reason
                    // this was owed: the world has to be the one the flip two
                    // steps down will land into, not one read on a separate
                    // connection that a concurrent publish can invalidate in
                    // between.
                    //
                    // **Before the registry request**, on step 3's own argument —
                    // a refusal after the handle is issued leaves a pending
                    // version nothing ever commits and trips `commit_overdue` for
                    // a publish that never happened.
                    let world = overlay_repo::world_on(txn, &scope, tenant_id, &record)
                        .await
                        .map_err(|e| repo_failure(&e))?;
                    let report = validate(&OverlayCandidate {
                        price_overlay_id: record.price_overlay_id,
                        revision: record.revision,
                        scope: record.scope.clone(),
                        precedence: record.precedence,
                        interval: record.interval,
                        tax_basis: record.tax_basis,
                        disclosure: record.disclosure,
                        target_ref: record.target_ref.clone(),
                        lines: record.lines.clone(),
                        world,
                    });
                    // The same two-tier shape the submit uses: §5 types these two
                    // codes 409 and the rest architectural 422s, so a conflict is
                    // lifted out of the envelope rather than buried in it.
                    // Warnings do not block — `EQUAL_PRECEDENCE_CROSS_CLASS_TIE`
                    // and `FIXED_LINE_DISCARDS_STACK` are things an operator is
                    // told, not things a commit refuses.
                    if let Some(violation) = conflict_of(&report) {
                        return Err(match violation.code.as_str() {
                            PRECEDENCE_DUPLICATE => {
                                DomainError::PrecedenceDuplicate(violation.detail.clone())
                            }
                            _ => DomainError::OverlayIntervalOverlap(violation.detail.clone()),
                        });
                    }
                    if !report.is_publishable() {
                        return Err(DomainError::ValidationFailed(report));
                    }

                    // 2. The shards, resolved before the flip moves the
                    // predecessor out of `published`.
                    let subjects =
                        subjects_of(txn, &scope, tenant_id, price_overlay_id, &record).await?;

                    // 3. Addressability, fail closed.
                    let pending = request_version_now(registry.as_ref(), &ctx, &request_id).await?;

                    // 4. The flip, its predecessor's supersession and the audit
                    // record, which `overlay_repo` keeps as one act.
                    overlay_repo::publish_revision_on(
                        txn,
                        &scope,
                        tenant_id,
                        price_overlay_id,
                        revision,
                        stamp,
                    )
                    .await
                    .map_err(|e| repo_failure(&e))?;

                    // 5. One ref per subject, all on the one handle.
                    for subject in &subjects {
                        let revisioned = matches!(subject, SubjectRef::PriceOverlay(_));
                        catalog_version_ref_repo::record_pending(
                            txn,
                            &scope,
                            PendingVersionRow::for_subject(
                                tenant_id,
                                pending.pending_ref.clone(),
                                subject,
                                revisioned.then_some(revision),
                                // A shard has no lifecycle of its own; the
                                // document's rides inside the revision it freezes.
                                revisioned
                                    .then_some(crate::domain::lifecycle::LifecycleState::Published),
                                stamp.recorded_at,
                            ),
                        )
                        .await
                        .map_err(|e| repo_failure(&e))?;
                    }

                    // 6. The announcement (D-248). Every other projected
                    // subject that moves says so on the wire — a plan publish, a
                    // bundle, a price, a window — and until this line an overlay
                    // publish moved **two** projected subjects and announced
                    // neither, so a consumer learned that everything underneath the
                    // overlay layer had changed and never that the layer had.
                    //
                    // **The aggregate is the overlay, not a plan**, which is the one
                    // place this differs from `BundleUpdated`: a bundle rides a plan
                    // revision, while an overlay is tenant-scoped and may target no
                    // plan at all, so there is no plan stream to order it within.
                    // Per-overlay is also the right granularity — two revisions of
                    // one overlay must not be observed out of order, and two
                    // different overlays have no ordering relationship to keep.
                    //
                    // The name, the aggregate, the wire keys and the dedup key
                    // are `NewOutboxEvent::price_overlay_published`'s and never
                    // this call site's — Z8-4. Until then this was one of the two
                    // from-scratch `NewOutboxEvent` literals in the crate, which
                    // is how the event name came to be hand-spelled in a
                    // `format!` two lines under the enum that renders it.
                    outbox_repo::enqueue(
                        txn,
                        &scope,
                        NewOutboxEvent::price_overlay_published(
                            tenant_id,
                            &PriceOverlayPublishedPayload {
                                price_overlay_id,
                                revision,
                                scope: record.scope.clone(),
                                precedence: record.precedence,
                                pending_version_ref: pending.pending_ref.clone(),
                                correlation_id: stamp.correlation_id,
                            },
                            stamp.recorded_at,
                        ),
                    )
                    .await
                    .map_err(|e| repo_failure(&e))?;

                    // 7. The units this publish did not consume.
                    approval_repo::void_pending_for_subject(
                        txn,
                        &scope,
                        tenant_id,
                        &audit_repo::overlay_revision_ref(price_overlay_id, revision),
                        crate::infra::approval::ORPHANED_BY_PUBLISH_REASON,
                        stamp.recorded_at,
                    )
                    .await
                    .map_err(|e| repo_failure(&e))?;

                    Ok(OverlayPublishReceipt {
                        price_overlay_id,
                        revision,
                        pending_ref: pending.pending_ref,
                        subjects,
                    })
                })
            })
            .await;
        outcome.map_err(|err| {
            err.into_domain(|infra| {
                DomainError::Internal(format!("bss-pricing: overlay publish transaction: {infra}"))
            })
        })
    }
}

/// Every subject this act projects: the document, and the shard or shards it
/// rewrites.
///
/// **Two shards when a revision moves the scope value** (D-133), and the
/// predecessor is read **before** the flip for that reason: afterwards it stands
/// `superseded` and the shard it is leaving would have to be reconstructed from a
/// state that has already moved. A revision that keeps its scope names one shard,
/// and the set is deduplicated rather than assumed distinct.
///
/// # Errors
/// [`DomainError::Internal`] on a storage failure, or on a stored scope that is
/// not a shard key.
async fn subjects_of(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    price_overlay_id: Uuid,
    draft: &overlay_repo::OverlayRecord,
) -> Result<Vec<SubjectRef>, DomainError> {
    let mut shards = vec![OverlayIndexShard::try_from(&draft.scope)?];
    if let Some(current) = overlay_repo::current_on(runner, scope, tenant_id, price_overlay_id)
        .await
        .map_err(|e| repo_failure(&e))?
    {
        let leaving = OverlayIndexShard::try_from(&current.scope)?;
        if !shards.contains(&leaving) {
            shards.push(leaving);
        }
    }
    let mut subjects = vec![SubjectRef::PriceOverlay(price_overlay_id)];
    subjects.extend(shards.into_iter().map(SubjectRef::OverlayIndex));
    Ok(subjects)
}

/// The registry request id of one overlay publish.
///
/// **Deterministic, and that is the whole point** — [`crate::infra::publish`]'s
/// `publish_request_id` states the argument in full: `request_id` is the
/// catalog's own idempotency handle, so a commit that rolled back and is retried
/// must present the id its first attempt did, or every rollback orphans a pending
/// ref and each orphan trips `commit_overdue` for a publish that never happened.
///
/// The token is the overlay plane's own rather than a `PublishUnitKind`'s: this
/// is not one of that enum's units, which is D-234 in one line.
#[must_use]
fn publish_request_id(tenant_id: Uuid, price_overlay_id: Uuid, revision: u64) -> String {
    format!("overlay-content/{tenant_id}/{price_overlay_id}/{revision}")
}
