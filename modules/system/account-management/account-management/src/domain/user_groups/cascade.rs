//! Cascade cleanup hook for tenant hard-delete.
//!
//! Implements `cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use modkit_odata::ast::{CompareOperator, Expr, Value};
use modkit_odata::{CursorV1, ODataQuery};
use modkit_security::SecurityContext;
use resource_group_sdk::{ResourceGroupClient, ResourceGroupError};
use tracing::{debug, info};
use uuid::Uuid;

use super::{USER_GROUP_TYPE_CODE, am_system_context};
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::domain::tenant::hooks::{HookError, TenantHardDeleteHook};

/// Default timeout for a single RG round-trip.
const CASCADE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum groups to fetch per page during cascade.
const CASCADE_PAGE_SIZE: u64 = 100;

/// Overall budget per cascade invocation. Caps the worst-case time the
/// retention pipeline can spend on a single tenant before returning
/// [`HookError::Retryable`] so the next tick resumes. Without this an
/// O(N^2) parent/child retry loop combined with the 10s per-call
/// `CASCADE_TIMEOUT` could keep one tenant blocking the pipeline for
/// hours.
#[allow(
    clippy::duration_suboptimal_units,
    reason = "from_mins is unstable on workspace MSRV; keep from_secs"
)]
const CASCADE_BUDGET: Duration = Duration::from_secs(120);

// @cpt-begin:cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger:p1:inst-flow-cascade-hook
/// Build the cascade cleanup hook that deletes a tenant's user-group
/// subtree via `ResourceGroupClient` during hard-delete.
///
/// The hook:
/// 1. Lists all groups of type `USER_GROUP_TYPE_CODE` scoped to
///    `tenant_id`.
/// 2. For each listed group: invokes
///    [`ResourceGroupClient::delete_group_cascade`] (RG's `force=true`
///    variant), which atomically tears down the group + its subtree +
///    every membership row + every closure-table row anchored at it,
///    in a single SERIALIZABLE transaction on the RG side. AM does
///    **not** drain memberships separately, and does **not** run the
///    multi-pass leaf-first retry loop that older revisions of this
///    hook implemented -- both responsibilities now live in RG.
/// 3. Per-call `NotFound` is treated as idempotent success (a parent's
///    cascade may already have eaten the descendant, or a peer cleanup
///    tick may have raced this run).
///
/// If RG is unreachable, the hook returns [`HookError::Retryable`] so
/// the pipeline defers the tenant to the next retention tick. The
/// entire body is wrapped in a single [`tokio::time::timeout`]
/// (`CASCADE_BUDGET`) so a single misbehaving tenant cannot monopolise
/// the retention pipeline indefinitely.
pub fn build_cascade_cleanup_hook(
    client: Arc<dyn ResourceGroupClient + Send + Sync>,
) -> TenantHardDeleteHook {
    Arc::new(move |tenant_id: Uuid| {
        let client = Arc::clone(&client);
        async move {
            // Overall budget — hit means we yield to the next retention
            // tick instead of monopolising the pipeline.
            match tokio::time::timeout(CASCADE_BUDGET, cascade_inner(&client, tenant_id)).await {
                Ok(res) => res,
                Err(_elapsed) => {
                    emit_metric(
                        AM_DEPENDENCY_HEALTH,
                        MetricKind::Counter,
                        &[
                            ("target", "resource_group"),
                            ("op", "cascade_cleanup"),
                            ("outcome", "budget_exceeded"),
                        ],
                    );
                    Err(retryable(format!(
                        "cascade cleanup for tenant {tenant_id} exceeded overall budget of \
                         {}s; retrying on next retention tick",
                        CASCADE_BUDGET.as_secs()
                    )))
                }
            }
        }
        .boxed()
    })
}
// @cpt-end:cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger:p1:inst-flow-cascade-hook

/// Inner cascade body, separated so the overall budget timeout in the
/// hook closure can wrap it as one future.
///
/// Each tenant-bound user-group is handed off to RG's
/// [`ResourceGroupClient::delete_group_cascade`] (the SDK's
/// `force=true` variant), which atomically tears down the group + its
/// subtree + every membership row + every closure row in a single
/// SERIALIZABLE transaction on the RG side. AM no longer drains
/// memberships or replays multi-pass deletes -- both responsibilities
/// move to RG where the SQL-level cascade is already implemented.
///
/// `NotFound` per call is treated as idempotent success: a parent's
/// cascade may have already removed a descendant before we reach it,
/// or a peer cleanup tick may have raced this run. The order in
/// which `fetch_tenant_groups` returns rows is irrelevant for the
/// same reason -- cascading from a parent eats its descendants.
async fn cascade_inner(
    client: &Arc<dyn ResourceGroupClient + Send + Sync>,
    tenant_id: Uuid,
) -> Result<(), HookError> {
    let start = Instant::now();
    // System-actor context bound to the tenant being deleted. A future
    // RG-side authz tightening that rejects `SecurityContext::anonymous`
    // would not regress this hook because the subject is a stable
    // platform-root UUID.
    let ctx = am_system_context(Some(tenant_id));

    let groups = fetch_tenant_groups(client, &ctx, tenant_id).await?;

    if groups.is_empty() {
        // Emit the success metric on the empty-tenant path too so the
        // counter "tenant cleanup completed without error" stays
        // symmetric with the non-empty path below — the success
        // counter must fire on both terminal branches of
        // `cascade_inner` so operators charting cleanup-success rate
        // get a single unified signal.
        emit_metric(
            AM_DEPENDENCY_HEALTH,
            MetricKind::Counter,
            &[
                ("target", "resource_group"),
                ("op", "cascade_cleanup"),
                ("outcome", "success"),
            ],
        );
        info!(
            target: "am.user_groups",
            tenant_id = %tenant_id,
            groups_deleted = 0,
            elapsed_ms = start.elapsed().as_millis(),
            "cascade cleanup completed (no groups)"
        );
        return Ok(());
    }

    let total_groups = groups.len();
    let mut deleted: usize = 0;
    let mut already_deleted: usize = 0;

    for group_id in &groups {
        if cascade_one(client, &ctx, tenant_id, *group_id).await? {
            already_deleted += 1;
        } else {
            deleted += 1;
        }
    }

    emit_metric(
        AM_DEPENDENCY_HEALTH,
        MetricKind::Counter,
        &[
            ("target", "resource_group"),
            ("op", "cascade_cleanup"),
            ("outcome", "success"),
        ],
    );
    info!(
        target: "am.user_groups",
        tenant_id = %tenant_id,
        groups_listed = total_groups,
        groups_deleted = deleted,
        already_deleted,
        elapsed_ms = start.elapsed().as_millis(),
        "cascade cleanup completed for tenant user groups"
    );

    Ok(())
}

/// Issue a single `delete_group_cascade` call.
///
/// Returns `Ok(true)` when RG reports `NotFound` (idempotent skip),
/// `Ok(false)` on a successful delete. Any other RG error or a
/// per-call timeout becomes [`HookError::Retryable`] so the
/// retention pipeline resumes on the next tick.
async fn cascade_one(
    client: &Arc<dyn ResourceGroupClient + Send + Sync>,
    ctx: &SecurityContext,
    tenant_id: Uuid,
    group_id: Uuid,
) -> Result<bool, HookError> {
    match tokio::time::timeout(CASCADE_TIMEOUT, client.delete_group_cascade(ctx, group_id)).await {
        Err(_elapsed) => {
            emit_metric(
                AM_DEPENDENCY_HEALTH,
                MetricKind::Counter,
                &[
                    ("target", "resource_group"),
                    ("op", "cascade_delete_group"),
                    ("outcome", "timeout"),
                ],
            );
            Err(retryable("timeout deleting group"))
        }
        Ok(Err(ResourceGroupError::NotFound { .. })) => {
            // Already removed by a previous cascade step (parent ate
            // the descendant) or a peer cleanup tick. Idempotent
            // skip, observable so a future RG-side authz tightening
            // that mis-classifies as `NotFound` does not silently
            // leave orphaned closure / membership rows.
            emit_metric(
                AM_DEPENDENCY_HEALTH,
                MetricKind::Counter,
                &[
                    ("target", "resource_group"),
                    ("op", "cascade_delete_group"),
                    ("outcome", "already_deleted"),
                ],
            );
            debug!(
                target: "am.user_groups",
                tenant_id = %tenant_id,
                group_id = %group_id,
                "cascade-delete returned NotFound; treating as already-deleted"
            );
            Ok(true)
        }
        Ok(Err(e)) => {
            emit_metric(
                AM_DEPENDENCY_HEALTH,
                MetricKind::Counter,
                &[
                    ("target", "resource_group"),
                    ("op", "cascade_delete_group"),
                    ("outcome", "error"),
                ],
            );
            Err(retryable(format!(
                "failed to cascade-delete group {group_id}: {e}"
            )))
        }
        Ok(Ok(())) => Ok(false),
    }
}

/// Fetch root user-group RG entries scoped to `tenant_id`, draining
/// all pages.
///
/// Filters to `parent_id IS NULL` (roots only): `delete_group_cascade`
/// on a root atomically removes the entire subtree on the RG side, so
/// listing descendants would only produce redundant `NotFound`
/// responses that burn `CASCADE_BUDGET` for no work. AM's registration
/// pins `allowed_parent_types = [USER_GROUP_TYPE_CODE]` (a user-group
/// may only parent another user-group of the same type), so every
/// descendant of a root user-group in the tenant is itself a
/// user-group — hence reachable through its root's cascade. This
/// invariant rests on two pillars: AM is the sole writer of
/// user-group rows (it only ever passes another user-group's UUID as
/// `parent_id` when creating one), and RG enforces
/// `allowed_parent_types` at `create_group` time — so even an RG seed
/// with broader `allowed_parent_types` (which `classify_existing`
/// admits per the looseness contract) cannot produce a user-group
/// with a non-user-group parent unless AM itself requests one, which
/// it does not.
async fn fetch_tenant_groups(
    client: &Arc<dyn ResourceGroupClient + Send + Sync>,
    ctx: &SecurityContext,
    tenant_id: Uuid,
) -> Result<Vec<Uuid>, HookError> {
    // tenant_id eq T AND type eq USER_GROUP AND hierarchy/parent_id eq null
    let filter = Expr::And(
        Box::new(Expr::And(
            Box::new(Expr::Compare(
                Box::new(Expr::Identifier("tenant_id".to_owned())),
                CompareOperator::Eq,
                Box::new(Expr::Value(Value::Uuid(tenant_id))),
            )),
            Box::new(Expr::Compare(
                Box::new(Expr::Identifier("type".to_owned())),
                CompareOperator::Eq,
                Box::new(Expr::Value(Value::String(USER_GROUP_TYPE_CODE.to_owned()))),
            )),
        )),
        Box::new(Expr::Compare(
            Box::new(Expr::Identifier("hierarchy/parent_id".to_owned())),
            CompareOperator::Eq,
            Box::new(Expr::Value(Value::Null)),
        )),
    );

    let mut all_ids = Vec::new();
    let mut cursor: Option<CursorV1> = None;

    loop {
        let mut query = ODataQuery::default()
            .with_limit(CASCADE_PAGE_SIZE)
            .with_filter(filter.clone());
        if let Some(c) = cursor.take() {
            query = query.with_cursor(c);
        }

        let page =
            match tokio::time::timeout(CASCADE_TIMEOUT, client.list_groups(ctx, &query)).await {
                Err(_elapsed) => {
                    emit_metric(
                        AM_DEPENDENCY_HEALTH,
                        MetricKind::Counter,
                        &[
                            ("target", "resource_group"),
                            ("op", "cascade_list_groups"),
                            ("outcome", "timeout"),
                        ],
                    );
                    return Err(retryable("resource-group: timeout listing user groups"));
                }
                Ok(Err(e)) => {
                    emit_metric(
                        AM_DEPENDENCY_HEALTH,
                        MetricKind::Counter,
                        &[
                            ("target", "resource_group"),
                            ("op", "cascade_list_groups"),
                            ("outcome", "error"),
                        ],
                    );
                    return Err(retryable(format!("resource-group: {e}")));
                }
                Ok(Ok(p)) => p,
            };

        all_ids.extend(page.items.into_iter().map(|g| g.id));

        match page.page_info.next_cursor {
            Some(token) => {
                cursor = Some(
                    CursorV1::decode(&token)
                        .map_err(|e| retryable(format!("invalid cursor from RG: {e}")))?,
                );
            }
            None => break,
        }
    }

    Ok(all_ids)
}

fn retryable(detail: impl Into<String>) -> HookError {
    HookError::Retryable {
        detail: detail.into(),
    }
}
