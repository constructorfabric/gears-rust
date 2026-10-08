//! Background publication after wiring (SPEC §10.1, D16, D18, D21; T33).
//! Resolve the client each cycle; retry pending declarations with bounded passes and jitter.
//! Rejected/superseded identifiers are never resubmitted; registry problems log on change.
//! Gears supply their own `PublisherContext`; transports apply Retry-After. Until T37/T38,
//! synchronous local reconciliation coexists with this path.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use crate::publication::{
    GtsDeclaration, PendingReason, PublicationState, PublicationStatus, PublisherContext,
    PublisherVersion, RejectionReason, SupersededEntity,
};
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit::tokio::time::{Instant, sleep_until};
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};
use toolkit_security::PlatformSecurityContext;

use crate::contract::PlatformTypesRegistryApi;
use crate::ext::jittered;
use crate::ext::{MAX_BATCH_GET_KEYS, bounded, deadline_from_now};
use crate::item_failure::{AdmissionFailure, context, reason};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetItem, EntityKey, EntityLookup, FieldSelection,
    LifecycleStatus, Projection,
};
use crate::publication::reconcile::{
    Liveness, Outcome, PendingCause, ReconcileOptions, Reconciliation, reconcile,
};
use crate::publication::supervised::{StatusReporter, Supervised, spawn_supervised};

/// Minimum cycle pause, including jitter, prevents instant failures from spinning.
pub const MIN_CYCLE_BACKOFF: Duration = Duration::from_millis(50);

/// Tuning for [`publish_gts_with`]; initialize with Default, then adjust fields.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PublishOptions {
    /// Per-cycle reconciliation; passes bound dependency retries.
    pub reconcile: ReconcileOptions,
    /// Cycle backoff doubles to `cycle_backoff_max`; clamp and jitter without crossing
    /// [`MIN_CYCLE_BACKOFF`].
    pub cycle_backoff: Duration,
    /// Longest pause between two cycles; never below [`MIN_CYCLE_BACKOFF`].
    pub cycle_backoff_max: Duration,
}

impl Default for PublishOptions {
    fn default() -> Self {
        Self {
            reconcile: ReconcileOptions {
                passes: std::num::NonZeroU32::new(3).unwrap_or(std::num::NonZeroU32::MIN),
                ..ReconcileOptions::default()
            },
            cycle_backoff: Duration::from_secs(1),
            cycle_backoff_max: Duration::from_secs(60),
        }
    }
}

/// Publishes `declarations` under `publisher` in a supervised background task.
#[must_use]
pub fn publish_gts(
    hub: Arc<ClientHub>,
    publisher: PublisherContext,
    declarations: Vec<GtsDeclaration>,
    cancel: CancellationToken,
) -> Supervised<PublicationStatus> {
    publish_gts_with(
        hub,
        publisher,
        declarations,
        cancel,
        PublishOptions::default(),
    )
}

/// [`publish_gts`] with explicit tuning.
#[must_use]
pub fn publish_gts_with(
    hub: Arc<ClientHub>,
    publisher: PublisherContext,
    declarations: Vec<GtsDeclaration>,
    cancel: CancellationToken,
    options: PublishOptions,
) -> Supervised<PublicationStatus> {
    let initial = PublicationStatus::pending(declarations.iter().map(|(id, _)| id.clone()));
    spawn_supervised(
        format!("gts-publish:{}", publisher.name),
        initial,
        cancel,
        move |reporter, cancel| run(hub, publisher, declarations, options, reporter, cancel),
    )
}

/// Superseded but unverified: re-read liveness only, never resubmit.
struct Unverified {
    stored_version: PublisherVersion,
    offered_version: PublisherVersion,
}

async fn run(
    hub: Arc<ClientHub>,
    publisher: PublisherContext,
    declarations: Vec<GtsDeclaration>,
    options: PublishOptions,
    reporter: StatusReporter<PublicationStatus>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let ctx = PlatformSecurityContext::outbound_marker();
    let mut status = reporter.current();
    let mut unverified: BTreeMap<String, Unverified> = BTreeMap::new();
    let max_backoff = options.cycle_backoff_max.max(MIN_CYCLE_BACKOFF);
    let mut backoff = options.cycle_backoff.clamp(MIN_CYCLE_BACKOFF, max_backoff);
    let mut reported_problem: Option<String> = None;
    loop {
        let unsettled: Vec<&GtsDeclaration> = declarations
            .iter()
            .filter(|(id, _)| {
                !unverified.contains_key(id)
                    && status
                        .entities
                        .get(id)
                        .is_none_or(|state| matches!(state, PublicationState::Pending(_)))
            })
            .collect();

        match hub.get::<dyn PlatformTypesRegistryApi>() {
            Err(e) => {
                let message = format!("the registry client is not available: {e}");
                let pending = unsettled.iter().map(|(id, _)| id).chain(unverified.keys());
                for id in pending {
                    status.entities.insert(
                        id.clone(),
                        PublicationState::Pending(PendingReason::RegistryUnreachable {
                            message: message.clone(),
                        }),
                    );
                }
            }
            Ok(api) => {
                // Bound verification so a hung read cannot stall later cycles.
                let deadline = deadline_from_now(options.reconcile.deadline)?;
                verify(
                    api.as_ref(),
                    &ctx,
                    &mut unverified,
                    &mut status,
                    &publisher,
                    deadline,
                    &cancel,
                )
                .await?;
                if !unsettled.is_empty() {
                    let result = reconcile(
                        api.as_ref(),
                        &ctx,
                        &publisher,
                        unsettled.iter().copied(),
                        &options.reconcile,
                        &cancel,
                    )
                    .await;
                    match result {
                        Ok(Reconciliation::UpToDate) => {
                            for (id, state) in &mut status.entities {
                                if !unverified.contains_key(id)
                                    && matches!(state, PublicationState::Pending(_))
                                {
                                    *state = PublicationState::Admitted;
                                }
                            }
                        }
                        Ok(Reconciliation::Reconciled(outcomes)) => {
                            apply(&mut status, &mut unverified, outcomes, &publisher);
                        }
                        Err(CanonicalError::Cancelled { .. }) => {
                            anyhow::bail!("publication was cancelled");
                        }
                        Err(e) => anyhow::bail!("reconciliation could not run: {e}"),
                    }
                }
            }
        }
        reporter.report(status.clone());
        if status.is_terminal() {
            return Ok(());
        }

        // Jitter never undercuts the floor: a failing cycle must not spin.
        let wait = jittered(backoff).max(MIN_CYCLE_BACKOFF);
        report_registry_problem(&publisher, &status, &mut reported_problem);
        tracing::debug!(
            gear = %publisher.name,
            pending = status
                .entities
                .values()
                .filter(|state| matches!(state, PublicationState::Pending(_)))
                .count(),
            backoff = ?wait,
            "a publication cycle left identifiers pending"
        );
        // A pause no clock can represent waits for cancellation alone.
        let pause = async {
            match Instant::now().checked_add(wait) {
                Some(wake) => sleep_until(wake).await,
                None => std::future::pending().await,
            }
        };
        toolkit::tokio::select! {
            () = cancel.cancelled() => anyhow::bail!("publication was cancelled"),
            () = pause => {}
        }
        backoff = backoff.saturating_mul(2).min(max_backoff);
    }
}

/// Log registry problems only when they appear/change, and once on recovery.
fn report_registry_problem(
    publisher: &PublisherContext,
    status: &PublicationStatus,
    reported: &mut Option<String>,
) {
    let mut affected = 0_usize;
    let mut problem = None;
    for state in status.entities.values() {
        if let PublicationState::Pending(
            PendingReason::RegistryUnreachable { message }
            | PendingReason::RegistryRefused { message },
        ) = state
        {
            affected += 1;
            problem.get_or_insert(message);
        }
    }
    match problem {
        Some(message) if reported.as_deref() != Some(message.as_str()) => {
            tracing::warn!(
                gear = %publisher.name,
                pending = affected,
                %message,
                "GTS publication cannot reach the registry; it keeps retrying"
            );
            *reported = Some(message.clone());
        }
        Some(_) => {}
        None => {
            if reported.take().is_some() {
                tracing::info!(gear = %publisher.name, "GTS publication reaches the registry again");
            }
        }
    }
}

/// Settle superseded liveness on a valid read; otherwise leave it pending.
async fn verify<A: PlatformTypesRegistryApi + ?Sized>(
    api: &A,
    ctx: &PlatformSecurityContext,
    unverified: &mut BTreeMap<String, Unverified>,
    status: &mut PublicationStatus,
    publisher: &PublisherContext,
    deadline: Instant,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let keys: Vec<(String, EntityKey)> = unverified
        .keys()
        .filter_map(|id| Some((id.clone(), EntityKey::GtsId(gts::GtsId::try_new(id).ok()?))))
        .collect();
    for chunk in keys.chunks(MAX_BATCH_GET_KEYS) {
        let request = BatchGetEntitiesRequest {
            items: chunk
                .iter()
                .map(|(_, k)| BatchGetItem::from(k.clone()))
                .collect(),
            projection: Projection::Select(FieldSelection::light()),
            fresh: true,
        };
        let lookups: HashMap<EntityKey, EntityLookup> =
            match bounded(deadline, cancel, None, api.batch_get_entities(ctx, request)).await {
                Ok(Ok(lookups)) => lookups.0,
                Err(CanonicalError::Cancelled { .. }) => {
                    anyhow::bail!("publication was cancelled")
                }
                Ok(Err(e)) | Err(e) => {
                    mark_liveness_unread(status, publisher, chunk, e);
                    continue;
                }
            };
        for (id, key) in chunk {
            let Some(entity) = liveness_of(publisher, id, lookups.get(key)) else {
                continue;
            };
            if let Some(versions) = unverified.remove(id) {
                status.entities.insert(
                    id.clone(),
                    settled_superseded(id, versions, entity, publisher),
                );
            }
        }
    }
    Ok(())
}

/// Keeps the identifiers of a liveness read that failed pending, with its cause.
fn mark_liveness_unread(
    status: &mut PublicationStatus,
    publisher: &PublisherContext,
    chunk: &[(String, EntityKey)],
    error: CanonicalError,
) {
    // report_registry_problem emits the rate-limited warning from this status.
    tracing::debug!(
        gear = %publisher.name,
        unverified = chunk.len(),
        %error,
        "superseded liveness is still unverified; it is re-read next cycle"
    );
    let reason = match PendingCause::of(error) {
        PendingCause::Refused(e) => PendingReason::RegistryRefused {
            message: format!("superseded; the liveness read was refused: {}", e.detail()),
        },
        other => PendingReason::RegistryUnreachable {
            message: format!(
                "superseded; the liveness read failed: {}",
                other.error().detail()
            ),
        },
    };
    for (id, _) in chunk {
        status
            .entities
            .insert(id.clone(), PublicationState::Pending(reason.clone()));
    }
}

/// Classify superseded liveness; missing/unconditional-unchanged answers are protocol faults.
fn liveness_of(
    publisher: &PublisherContext,
    id: &str,
    lookup: Option<&EntityLookup>,
) -> Option<SupersededEntity> {
    match lookup {
        Some(EntityLookup::Found {
            entity: snapshot, ..
        }) => Some(if snapshot.lifecycle_status == LifecycleStatus::Deleted {
            SupersededEntity::Deleted
        } else {
            SupersededEntity::Live
        }),
        Some(EntityLookup::NotFound) => Some(SupersededEntity::Deleted),
        Some(EntityLookup::Unchanged { .. }) | None => {
            tracing::warn!(
                gear = %publisher.name,
                gts_id = id,
                "the registry did not answer a liveness read of this key; it is re-read next cycle"
            );
            None
        }
    }
}

/// Folds one reconciliation's outcomes into the status.
fn apply(
    status: &mut PublicationStatus,
    unverified: &mut BTreeMap<String, Unverified>,
    outcomes: BTreeMap<String, Outcome>,
    publisher: &PublisherContext,
) {
    for (id, outcome) in outcomes {
        let state = match outcome {
            Outcome::Superseded { error, liveness } => {
                superseded(&id, &error, liveness, unverified, publisher)
            }
            other => state_of(&id, other, publisher),
        };
        status.entities.insert(id, state);
    }
}

fn state_of(id: &str, outcome: Outcome, publisher: &PublisherContext) -> PublicationState {
    match outcome {
        Outcome::Admitted => PublicationState::Admitted,
        Outcome::Rejected(error) => {
            let rejection = rejection_of(&error);
            if let RejectionReason::Registry { reason, message }
            | RejectionReason::InvalidDeclaration { reason, message } = &rejection
            {
                tracing::warn!(
                    gear = %publisher.name,
                    gts_id = id,
                    reason,
                    message,
                    "a GTS declaration was rejected and will not be retried"
                );
            }
            PublicationState::Rejected(rejection)
        }
        Outcome::Pending(PendingCause::Dependency(error)) => {
            PublicationState::Pending(PendingReason::BlockedDependency {
                message: message_of(&error),
            })
        }
        Outcome::Pending(PendingCause::Conflict(error)) => {
            let message = message_of(&error);
            tracing::debug!(
                gear = %publisher.name,
                gts_id = id,
                %message,
                "a concurrent writer changed a declared GTS entity; it is re-read next cycle"
            );
            PublicationState::Pending(PendingReason::Conflict { message })
        }
        Outcome::Pending(PendingCause::Refused(error)) => {
            PublicationState::Pending(PendingReason::RegistryRefused {
                message: message_of(&error),
            })
        }
        Outcome::Pending(PendingCause::Unavailable(error)) | Outcome::Superseded { error, .. } => {
            PublicationState::Pending(PendingReason::RegistryUnreachable {
                message: message_of(&error),
            })
        }
    }
}

/// Reject missing/invalid superseded versions. Unverified liveness stays pending and is re-read,
/// never resubmitted.
fn superseded(
    id: &str,
    error: &CanonicalError,
    liveness: Liveness,
    unverified: &mut BTreeMap<String, Unverified>,
    publisher: &PublisherContext,
) -> PublicationState {
    let failure = AdmissionFailure::from_canonical(error);
    let version = |name: &str| -> Result<PublisherVersion, String> {
        let raw = failure
            .as_ref()
            .and_then(|f| f.context(name))
            .ok_or_else(|| format!("{name} was not reported"))?;
        raw.parse()
            .map_err(|e| format!("{name} '{}' is unusable: {e}", raw.escape_debug()))
    };
    let (stored_version, offered_version) = match (
        version(context::STORED_VERSION),
        version(context::OFFERED_VERSION),
    ) {
        (Ok(stored), Ok(offered)) => (stored, offered),
        (stored, offered) => {
            let why = [stored.err(), offered.err()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; ");
            tracing::warn!(
                gear = %publisher.name,
                gts_id = id,
                %why,
                "a superseded outcome named no usable versions; treated as rejected"
            );
            return PublicationState::Rejected(RejectionReason::Registry {
                reason: reason::SUPERSEDED.to_owned(),
                message: format!("superseded, but {why}"),
            });
        }
    };
    let versions = Unverified {
        stored_version,
        offered_version,
    };
    match liveness {
        Liveness::Live => settled_superseded(id, versions, SupersededEntity::Live, publisher),
        Liveness::Deleted => settled_superseded(id, versions, SupersededEntity::Deleted, publisher),
        Liveness::Unverified => {
            tracing::warn!(
                gear = %publisher.name,
                gts_id = id,
                "superseded, but its liveness could not be verified yet"
            );
            unverified.insert(id.to_owned(), versions);
            PublicationState::Pending(PendingReason::RegistryUnreachable {
                message: "superseded; whether the entity is live is not verified yet".to_owned(),
            })
        }
    }
}

fn settled_superseded(
    id: &str,
    versions: Unverified,
    entity: SupersededEntity,
    publisher: &PublisherContext,
) -> PublicationState {
    tracing::warn!(
        gear = %publisher.name,
        gts_id = id,
        stored_version = %versions.stored_version,
        offered_version = %versions.offered_version,
        ?entity,
        "a newer release of this publisher owns a declared GTS entity"
    );
    PublicationState::Superseded {
        stored_version: versions.stored_version,
        offered_version: versions.offered_version,
        entity,
    }
}

/// Use exact item/field-violation reasons; fall back to the error category.
fn rejection_of(error: &CanonicalError) -> RejectionReason {
    if let Some(failure) = AdmissionFailure::from_canonical(error) {
        return RejectionReason::Registry {
            reason: failure.reason.as_wire().to_owned(),
            message: failure.message,
        };
    }
    if let CanonicalError::InvalidArgument {
        ctx: InvalidArgument::FieldViolations { field_violations },
        ..
    } = error
        && let Some(violation) = field_violations.first()
    {
        return RejectionReason::InvalidDeclaration {
            reason: violation.reason.clone(),
            message: violation.description.clone(),
        };
    }
    RejectionReason::Registry {
        reason: error.title().to_owned(),
        message: error.detail().to_owned(),
    }
}

fn message_of(error: &CanonicalError) -> String {
    AdmissionFailure::from_canonical(error)
        .map_or_else(|| error.detail().to_owned(), |failure| failure.message)
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod publish_tests;
