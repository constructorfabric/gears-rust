//! Production [`PrincipalLifecycleNotifier`]: discovers the registered
//! principal-lifecycle hooks and delivers an event to each.
//!
//! Discovery mirrors plugin resolution elsewhere in AM (see
//! [`crate::infra::idp::lazy`]) with one difference: there is no vendor
//! selection. Every `PluginV1<PrincipalLifecycleHookSpecV1>` instance in the
//! types registry is a hook, and each one receives each event through the
//! scoped `ClientHub` client registered under its instance id.
//!
//! Hooks are resolved on every delivery, never cached. Deletions are rare,
//! so the extra registry listing costs nothing that matters, and resolving
//! late keeps the relative init order of AM and the hooks' gears irrelevant:
//! a snapshot taken at AM's init could miss a hook whose gear starts later.
//!
//! The listing is a platform read (types-registry SPEC §8.4), like `IdP`
//! plugin selection: which gears hook into AM is a process-wide fact, not
//! something the caller who triggered the deletion is entitled to see or
//! to narrow.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use account_management_sdk::{
    PrincipalLifecycleEvent, PrincipalLifecycleHookError, PrincipalLifecycleHookSpecV1,
    PrincipalLifecycleHookV1,
};
use async_trait::async_trait;
use futures::FutureExt;
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::{
    GtsId, GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
};

use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::domain::principal_lifecycle::PrincipalLifecycleNotifier;

/// Budget for the registry listing that discovers the hooks.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Budget for one hook's handling of one event. A hook that needs longer
/// (a large purge) is cancelled and fails as retryable; the retry carries
/// on from wherever the cancelled attempt left its state, since hooks are
/// idempotent and cancellation-safe.
const HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// Label value of the `target` dimension on [`AM_DEPENDENCY_HEALTH`].
const METRIC_TARGET: &str = "principal_lifecycle_hook";

/// Discovers hooks through the types registry and `ClientHub` and delivers
/// events to them. See the module docs.
pub struct HookDispatcher {
    hub: Arc<ClientHub>,
    registry: Arc<dyn PlatformTypesRegistryApi>,
}

impl HookDispatcher {
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, registry: Arc<dyn PlatformTypesRegistryApi>) -> Self {
        Self { hub, registry }
    }

    /// Every registered hook, in instance-id order so delivery order is
    /// deterministic.
    ///
    /// Fails — rather than skipping — on any hook it cannot reach: a hook
    /// that silently stopped receiving events would leave exactly the
    /// orphans hooks exist to remove. How it fails depends on whether time
    /// can fix the cause:
    ///
    /// * **Retryable** — the registry is unavailable or slow, or an
    ///   instance has no scoped client yet (its gear is typically still
    ///   starting). A later attempt can succeed.
    /// * **Terminal** — an instance that comes without its document, does
    ///   not parse as a hook instance, or whose content id disagrees with its
    ///   registry id. That is a broken deployment or broken data; retrying
    ///   cannot fix it, and it must reach an operator (a 500 on a user
    ///   delete, a parked tenant on a hard-delete) instead of being retried
    ///   forever.
    async fn resolve(
        &self,
    ) -> Result<Vec<(String, Arc<dyn PrincipalLifecycleHookV1>)>, PrincipalLifecycleHookError> {
        let type_id = PrincipalLifecycleHookSpecV1::gts_type_id();
        // The pattern is built from the spec's own static type id, so a
        // rejection is a defect in this crate, not a passing condition.
        let pattern = GtsIdPattern::try_new(type_id.as_ref()).map_err(|err| {
            terminal(format!(
                "hook spec type id {type_id} is not a valid pattern: {err}"
            ))
        })?;
        let ctx = PlatformSecurityContext::outbound_marker();
        let listing = self
            .registry
            .list_instances(&ctx, ListEntitiesRequest::matching(pattern.clone()));
        let mut instances = match tokio::time::timeout(DISCOVERY_TIMEOUT, listing).await {
            Ok(Ok(instances)) => instances,
            Ok(Err(err)) => {
                return Err(retryable(format!("types-registry unavailable: {err}")));
            }
            Err(_elapsed) => {
                return Err(retryable(format!(
                    "types-registry gave no answer within {DISCOVERY_TIMEOUT:?}"
                )));
            }
        };
        // Guard against a registry that ignores the pattern: a foreign
        // instance must not be mistaken for a hook. The guard must apply the
        // registry's own GTS matching, not a string prefix: the pattern
        // `…principal_lifecycle_hook.v1~` also matches compatible minor
        // versions such as `…principal_lifecycle_hook.v1.0~…`, which a
        // `starts_with` would drop silently — and a dropped hook is a
        // deletion that skips that hook's cleanup.
        instances.retain(|instance| {
            GtsId::try_new(instance.id.as_ref()).is_ok_and(|id| id.matches_pattern(&pattern))
        });
        instances.sort_by(|a, b| a.id.as_ref().cmp(b.id.as_ref()));

        let mut hooks = Vec::with_capacity(instances.len());
        for instance in instances {
            let gts_id = instance.id.as_ref().to_owned();
            // The typed list helper asks for documents, so an instance
            // without one is a registry defect, not an empty hook.
            let document = instance.content.ok_or_else(|| {
                terminal(format!("hook instance {gts_id} came without its document"))
            })?;
            let content: PluginV1<PrincipalLifecycleHookSpecV1> = serde_json::from_value(document)
                .map_err(|err| terminal(format!("hook instance {gts_id} does not parse: {err}")))?;
            if content.id.as_ref() != gts_id {
                return Err(terminal(format!(
                    "hook instance {gts_id} carries mismatched content id {}",
                    content.id.as_ref()
                )));
            }
            let hook = self
                .hub
                .try_get_scoped::<dyn PrincipalLifecycleHookV1>(&ClientScope::gts_id(&gts_id))
                .ok_or_else(|| {
                    retryable(format!(
                        "hook instance {gts_id} is registered but has no scoped client"
                    ))
                })?;
            hooks.push((gts_id, hook));
        }
        Ok(hooks)
    }
}

#[async_trait]
impl PrincipalLifecycleNotifier for HookDispatcher {
    async fn notify(
        &self,
        ctx: &SecurityContext,
        event: &PrincipalLifecycleEvent,
    ) -> Result<(), PrincipalLifecycleHookError> {
        let hooks = match self.resolve().await {
            Ok(hooks) => hooks,
            Err(err) => {
                record(event, "discovery_failed");
                tracing::warn!(
                    target: "am.principal_lifecycle",
                    event = event.kind(),
                    error = %err,
                    "principal-lifecycle hooks could not be resolved"
                );
                return Err(err);
            }
        };

        let mut worst: Option<PrincipalLifecycleHookError> = None;
        for (gts_id, hook) in hooks {
            let outcome = deliver(hook, ctx, event).await;
            match &outcome {
                Ok(()) => {
                    record(event, "ok");
                    tracing::debug!(
                        target: "am.principal_lifecycle",
                        event = event.kind(),
                        hook = %gts_id,
                        "principal-lifecycle hook handled event"
                    );
                }
                Err(err) => {
                    record(event, outcome_label(err));
                    tracing::warn!(
                        target: "am.principal_lifecycle",
                        event = event.kind(),
                        hook = %gts_id,
                        error = %err,
                        "principal-lifecycle hook failed"
                    );
                }
            }
            // Keep going: one failing hook must not starve the others, and
            // every hook sees the event again on retry anyway.
            if let Err(err) = outcome {
                worst = Some(match worst {
                    Some(prev @ PrincipalLifecycleHookError::Terminal { .. }) => prev,
                    _ => err,
                });
            }
        }
        worst.map_or(Ok(()), Err)
    }
}

/// Run one hook inline, under [`HOOK_TIMEOUT`].
///
/// The hook's future is polled here rather than spawned, so it never
/// outlives the delivery: when the timeout fires, or the caller itself is
/// dropped (a client gone mid-request, AM shutting down), the future is
/// dropped and the hook stops at its current await point. A spawned task
/// would keep running detached instead, and every retry or retention tick
/// could start another copy beside it — work no caller's concurrency
/// limit would bound. The SDK contract makes hooks cancellation-safe for
/// exactly this reason.
///
/// `catch_unwind` contains a panicking hook: it surfaces as a terminal
/// failure of that hook instead of unwinding through AM — which, on the
/// retention pipeline, would take down AM's background tasks with it.
async fn deliver(
    hook: Arc<dyn PrincipalLifecycleHookV1>,
    ctx: &SecurityContext,
    event: &PrincipalLifecycleEvent,
) -> Result<(), PrincipalLifecycleHookError> {
    // `AssertUnwindSafe`: after a panic AM touches nothing the hook may
    // have left half-updated; it only reports the failure. The hook's own
    // state is the hook's concern, as with any panic it raises.
    let call = AssertUnwindSafe(hook.on_event(ctx, event)).catch_unwind();
    match tokio::time::timeout(HOOK_TIMEOUT, call).await {
        Ok(Ok(result)) => result,
        Ok(Err(_panic)) => Err(PrincipalLifecycleHookError::Terminal {
            detail: "hook panicked".to_owned(),
        }),
        // The hook's future was dropped with the timeout, so it is no
        // longer running.
        Err(_elapsed) => Err(retryable(format!(
            "hook gave no answer within {HOOK_TIMEOUT:?} and was cancelled"
        ))),
    }
}

fn retryable(detail: String) -> PrincipalLifecycleHookError {
    PrincipalLifecycleHookError::Retryable { detail }
}

fn terminal(detail: String) -> PrincipalLifecycleHookError {
    PrincipalLifecycleHookError::Terminal { detail }
}

/// Metric label for a hook failure.
fn outcome_label(err: &PrincipalLifecycleHookError) -> &'static str {
    match err {
        PrincipalLifecycleHookError::Terminal { .. } => "terminal",
        _ => "retryable",
    }
}

/// One [`AM_DEPENDENCY_HEALTH`] sample per hook delivery or failed discovery.
fn record(event: &PrincipalLifecycleEvent, outcome: &str) {
    emit_metric(
        AM_DEPENDENCY_HEALTH,
        MetricKind::Counter,
        &[
            ("target", METRIC_TARGET),
            ("op", event.kind()),
            ("outcome", outcome),
        ],
    );
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "principal_lifecycle_tests.rs"]
mod principal_lifecycle_tests;
