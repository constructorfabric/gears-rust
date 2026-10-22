//! The decision path over in-memory fakes and the real Rego backend.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use admission_control_sdk::{
    EngineFailure, EngineFailureCondition, EngineResult, MAX_ENGINE_FINDINGS,
};
use policy_engine_sdk::reason;
use tenant_resolver_sdk::{BarrierMode, TenantId};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::test_support::{
    CHILD, DENY_ALL, DENY_DELETE, DENY_NONE, EXPLICIT_NULL, FakeBindings, GADGET, ISLAND,
    NOT_BOOLEAN, PARENT, SIBLING, Stack, TEST_PATTERN, WIDGET, binding, ctx_in, document, request,
    request_for, unbounded, widget_binding,
};
use super::{BuiltinPolicySet, CompileCache, DecisionService, NoopDecisionMetrics};
use crate::domain::ports::{BindingSource, HierarchyPort, PortError};

fn denials(result: &EngineResult) -> Vec<String> {
    match result {
        EngineResult::Deny { denials, .. } => {
            denials.iter().map(|d| d.document_name.clone()).collect()
        }
        _ => Vec::new(),
    }
}

fn shadows(result: &EngineResult) -> Vec<String> {
    match result {
        EngineResult::Permit { shadow_denials } | EngineResult::Deny { shadow_denials, .. } => {
            shadow_denials
                .iter()
                .map(|d| d.document_name.clone())
                .collect()
        }
        _ => Vec::new(),
    }
}

fn permit() -> EngineResult {
    EngineResult::Permit {
        shadow_denials: Vec::new(),
    }
}

fn failed_with(result: &Result<EngineResult, EngineFailure>, condition: EngineFailureCondition) {
    match result {
        Err(failure) => assert_eq!(failure.condition, condition, "{failure}"),
        Ok(result) => panic!("expected a {condition} failure, got {result:?}"),
    }
}

#[tokio::test]
async fn no_assignment_permits() {
    let stack = Stack::new(Vec::new());
    let verdict = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await
        .unwrap();
    assert_eq!(verdict, permit());
}

#[tokio::test]
async fn an_anonymous_context_is_an_invalid_request() {
    let stack = Stack::new(Vec::new());
    let result = stack
        .service
        .decide(&SecurityContext::anonymous(), &request("create", CHILD))
        .await;
    failed_with(&result, EngineFailureCondition::InvalidRequest);
}

#[tokio::test]
async fn an_unreachable_resource_tenant_is_a_boundary_denial_before_any_content_is_read() {
    let stack = Stack::new(vec![widget_binding(1, CHILD, true, DENY_NONE)]);
    // A sibling subtree, and a tenant behind a barrier from its parent.
    for (caller, resource) in [(SIBLING, CHILD), (PARENT, ISLAND), (CHILD, PARENT)] {
        let verdict = stack
            .service
            .decide(&ctx_in(caller), &request("create", resource))
            .await
            .unwrap();
        assert_eq!(verdict, EngineResult::deny(reason::TENANT_BOUNDARY));
    }
    assert!(stack.bindings.asked.lock().is_empty());
}

#[tokio::test]
async fn the_resource_tenant_itself_and_its_ancestors_are_reachable() {
    let stack = Stack::new(Vec::new());
    for caller in [CHILD, PARENT] {
        let verdict = stack
            .service
            .decide(&ctx_in(caller), &request("create", CHILD))
            .await
            .unwrap();
        assert!(
            matches!(verdict, EngineResult::Permit { .. }),
            "caller {caller}"
        );
    }
    assert_eq!(
        stack.hierarchy.reach_calls.load(Ordering::SeqCst),
        0,
        "reachability is read from the chain, not asked separately"
    );
}

/// A hierarchy whose answers disagree, as two reads around a barrier raised
/// between them would: the parent still reaches the child, but the child's
/// chain already stops at the child.
struct SplitHierarchy;

#[async_trait::async_trait]
impl HierarchyPort for SplitHierarchy {
    async fn ancestors(
        &self,
        _ctx: &SecurityContext,
        _tenant: TenantId,
    ) -> Result<Vec<Uuid>, PortError> {
        Ok(vec![CHILD])
    }

    async fn is_reachable(
        &self,
        _ctx: &SecurityContext,
        _from_tenant: TenantId,
        _target_tenant: TenantId,
        _barrier: BarrierMode,
    ) -> Result<bool, PortError> {
        Ok(true)
    }
}

#[tokio::test]
async fn reachability_comes_from_the_chain_that_selects_the_policy() {
    let bindings = Arc::new(FakeBindings::new(vec![widget_binding(
        1, PARENT, true, DENY_ALL,
    )]));
    let service = DecisionService::new(
        BuiltinPolicySet::default(),
        Arc::new(SplitHierarchy),
        Arc::clone(&bindings) as Arc<dyn BindingSource>,
        Arc::new(CompileCache::new(4)),
        Duration::from_secs(5),
        Arc::new(NoopDecisionMetrics),
    );
    // Neither consistent state permits: before the barrier the parent's
    // policy denies, after it the tenant boundary does.
    let verdict = service
        .decide(&ctx_in(PARENT), &request("create", CHILD))
        .await
        .unwrap();
    assert_eq!(verdict, EngineResult::deny(reason::TENANT_BOUNDARY));
    assert!(bindings.asked.lock().is_empty());
}

#[tokio::test]
async fn an_assignment_whose_owner_is_now_behind_a_barrier_is_dormant() {
    // PARENT assigned a deny-all bundle to ISLAND, which then became
    // self-managed: ISLAND's chain no longer contains PARENT.
    let stack = Stack::new(vec![
        widget_binding(1, ISLAND, true, DENY_ALL).owned_by(PARENT),
        widget_binding(2, CHILD, true, DENY_ALL).owned_by(PARENT),
    ]);
    let verdict = stack
        .service
        .decide(&ctx_in(ISLAND), &request("create", ISLAND))
        .await
        .unwrap();
    assert_eq!(verdict, permit());
    // The same owner still governs a tenant it reaches.
    let verdict = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await
        .unwrap();
    assert_eq!(denials(&verdict), ["doc2"]);
}

#[tokio::test]
async fn an_assignment_owned_below_its_target_never_applies() {
    let stack = Stack::new(vec![
        widget_binding(1, PARENT, true, DENY_ALL).owned_by(CHILD),
    ]);
    let verdict = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await
        .unwrap();
    assert_eq!(verdict, permit());
}

#[tokio::test]
async fn assignments_apply_nearest_tenant_first_then_bundle_id() {
    let stack = Stack::new(vec![
        widget_binding(9, PARENT, true, DENY_ALL),
        widget_binding(7, CHILD, true, DENY_ALL),
        widget_binding(5, CHILD, true, DENY_ALL),
    ]);
    let verdict = stack
        .service
        .decide(&ctx_in(PARENT), &request("create", CHILD))
        .await
        .unwrap();
    assert_eq!(denials(&verdict), ["doc5", "doc7", "doc9"]);
    assert_eq!(
        stack.bindings.asked.lock().as_slice(),
        [vec![CHILD, PARENT, super::test_support::ROOT]]
    );
}

#[tokio::test]
async fn a_barrier_stops_the_chain() {
    let stack = Stack::new(vec![
        widget_binding(1, PARENT, true, DENY_ALL),
        widget_binding(2, ISLAND, true, DENY_NONE),
    ]);
    let verdict = stack
        .service
        .decide(&ctx_in(ISLAND), &request("create", ISLAND))
        .await
        .unwrap();
    assert_eq!(verdict, permit());
    assert_eq!(stack.bindings.asked.lock().as_slice(), [vec![ISLAND]]);
}

#[tokio::test]
async fn documents_apply_by_resource_type_and_action() {
    let documents = vec![
        document("widget-any", DENY_ALL, &[WIDGET], &[]),
        document("gadget-only", DENY_ALL, &[GADGET], &[]),
        document("wildcard", DENY_ALL, &[TEST_PATTERN], &["create"]),
        document("delete-only", DENY_ALL, &[WIDGET], &["delete"]),
    ];
    let stack = Stack::new(vec![binding(1, CHILD, true, documents)]);

    let create = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await
        .unwrap();
    assert_eq!(denials(&create), ["widget-any", "wildcard"]);

    let delete = stack
        .service
        .decide(&ctx_in(CHILD), &request("delete", CHILD))
        .await
        .unwrap();
    assert_eq!(denials(&delete), ["widget-any", "delete-only"]);

    let gadget = stack
        .service
        .decide(&ctx_in(CHILD), &request_for("update", GADGET, CHILD))
        .await
        .unwrap();
    assert_eq!(denials(&gadget), ["gadget-only"]);
}

#[tokio::test]
async fn a_denial_reads_the_request() {
    let stack = Stack::new(vec![widget_binding(1, CHILD, true, DENY_DELETE)]);
    let ctx = ctx_in(CHILD);
    let permitted = stack
        .service
        .decide(&ctx, &request("create", CHILD))
        .await
        .unwrap();
    assert!(matches!(permitted, EngineResult::Permit { .. }));
    let denied = stack
        .service
        .decide(&ctx, &request("delete", CHILD))
        .await
        .unwrap();
    assert!(matches!(
        &denied,
        EngineResult::Deny { reason_code, .. } if reason_code == reason::POLICY_DENIED
    ));
}

#[tokio::test]
async fn non_enforcing_denials_are_shadow_denials() {
    let stack = Stack::new(vec![
        widget_binding(1, CHILD, false, DENY_ALL),
        widget_binding(2, CHILD, true, DENY_NONE),
    ]);
    let ctx = ctx_in(CHILD);
    let verdict = stack
        .service
        .decide(&ctx, &request("create", CHILD))
        .await
        .unwrap();
    assert!(matches!(verdict, EngineResult::Permit { .. }));
    assert_eq!(shadows(&verdict), ["doc1"]);

    let stack = Stack::new(vec![
        widget_binding(1, CHILD, false, DENY_ALL),
        widget_binding(2, CHILD, true, DENY_ALL),
    ]);
    let verdict = stack
        .service
        .decide(&ctx, &request("create", CHILD))
        .await
        .unwrap();
    assert_eq!(denials(&verdict), ["doc2"]);
    assert_eq!(shadows(&verdict), ["doc1"]);
}

#[tokio::test]
async fn an_explicit_null_fails_closed_where_an_undefined_rule_permits() {
    let undefined = "package p\ndeny if input.action == \"delete\"";
    let stack = Stack::new(vec![widget_binding(1, CHILD, true, undefined)]);
    let verdict = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await
        .unwrap();
    assert_eq!(verdict, permit());

    let stack = Stack::new(vec![widget_binding(1, CHILD, true, EXPLICIT_NULL)]);
    let result = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await;
    failed_with(&result, EngineFailureCondition::Internal);
}

#[tokio::test]
async fn an_evaluation_error_fails_closed_even_beside_a_permit() {
    let stack = Stack::new(vec![
        widget_binding(1, CHILD, true, DENY_NONE),
        widget_binding(2, CHILD, false, NOT_BOOLEAN),
    ]);
    let result = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await;
    failed_with(&result, EngineFailureCondition::Internal);
}

#[tokio::test]
async fn hierarchy_timeouts_and_outages_are_failures_not_denials() {
    let stack = Stack::new(vec![widget_binding(1, CHILD, true, DENY_ALL)]);
    *stack.hierarchy.fault.lock() = Some(PortError::Timeout);
    let result = stack
        .service
        .decide(&ctx_in(PARENT), &request("create", CHILD))
        .await;
    failed_with(&result, EngineFailureCondition::Timeout);

    *stack.hierarchy.fault.lock() = Some(PortError::Unavailable("down".to_owned()));
    let result = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await;
    failed_with(&result, EngineFailureCondition::Unavailable);
}

#[tokio::test]
async fn compiled_versions_are_cached_by_version_id() {
    let stack = Stack::new(vec![widget_binding(1, CHILD, true, DENY_NONE)]);
    let ctx = ctx_in(CHILD);
    for _ in 0..3 {
        stack
            .service
            .decide(&ctx, &request("create", CHILD))
            .await
            .unwrap();
    }
    assert_eq!(stack.metrics.misses.load(Ordering::SeqCst), 1);
    assert_eq!(stack.metrics.hits.load(Ordering::SeqCst), 2);
    assert_eq!(stack.cache.len(), 1);
}

#[tokio::test]
async fn content_is_read_only_for_versions_the_cache_misses() {
    let stack = Stack::new(vec![
        widget_binding(1, CHILD, true, DENY_NONE),
        widget_binding(1, PARENT, true, DENY_NONE),
        widget_binding(2, PARENT, true, DENY_NONE),
    ]);
    let ctx = ctx_in(CHILD);
    for _ in 0..2 {
        stack
            .service
            .decide(&ctx, &request("create", CHILD))
            .await
            .unwrap();
    }
    let reads = stack.bindings.content_reads.lock().clone();
    assert_eq!(reads.len(), 1, "a warm cache reads no content: {reads:?}");
    assert_eq!(reads[0].len(), 2, "each missed version once: {reads:?}");
}

#[test]
fn concurrent_misses_of_one_version_compile_it_once() {
    let cache = Arc::new(super::CompileCache::new(4));
    let version = widget_binding(1, CHILD, true, DENY_NONE).version;
    let compilations = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(8));
    // Collected first: spawning lazily would join each thread before the
    // next starts.
    #[allow(clippy::needless_collect)]
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (cache, version, compilations, barrier) = (
                Arc::clone(&cache),
                Arc::clone(&version),
                Arc::clone(&compilations),
                Arc::clone(&barrier),
            );
            std::thread::spawn(move || {
                barrier.wait();
                cache
                    .get_or_compile(version.id, || {
                        compilations.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(20));
                        super::compile_version(&version)
                    })
                    .unwrap()
            })
        })
        .collect();
    let compiled: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(compilations.load(Ordering::SeqCst), 1);
    assert!(compiled.iter().all(|c| Arc::ptr_eq(c, &compiled[0])));
}

#[test]
fn a_failed_compilation_is_not_cached() {
    let cache = super::CompileCache::new(4);
    let version = widget_binding(1, CHILD, true, "package p\ndeny :=").version;
    for _ in 0..2 {
        assert!(
            cache
                .get_or_compile(version.id, || super::compile_version(&version))
                .is_err()
        );
    }
    assert!(cache.is_empty());
}

#[tokio::test]
async fn the_cache_is_bounded() {
    let cache = super::CompileCache::new(2);
    let bindings: Vec<_> = (1..=3)
        .map(|n| widget_binding(n, CHILD, true, DENY_NONE))
        .collect();
    for b in &bindings {
        let compiled = super::compile_version(&b.version).unwrap();
        cache.insert(b.version.id, std::sync::Arc::new(compiled));
    }
    assert_eq!(cache.len(), 2);
    assert!(
        cache.get(bindings[0].version.id).is_none(),
        "oldest evicted"
    );
    assert!(cache.get(bindings[2].version.id).is_some());
}

fn builtin_denial() -> EngineResult {
    EngineResult::deny(reason::BUILTIN_POLICY_DENIED)
}

#[tokio::test]
async fn a_builtin_denial_comes_before_the_tenant_boundary_and_any_content() {
    let stack = Stack::with_builtins(
        &[document("platform", DENY_ALL, &[WIDGET], &[])],
        vec![widget_binding(1, CHILD, true, DENY_NONE)],
    );
    for (caller, resource) in [(CHILD, CHILD), (SIBLING, CHILD)] {
        let verdict = stack
            .service
            .decide(&ctx_in(caller), &request("create", resource))
            .await
            .unwrap();
        assert_eq!(verdict, builtin_denial());
    }
    assert_eq!(stack.hierarchy.reach_calls.load(Ordering::SeqCst), 0);
    assert!(stack.bindings.asked.lock().is_empty());
}

#[tokio::test]
async fn builtins_reach_behind_a_barrier_where_tenant_policy_does_not() {
    let parent_policy = vec![widget_binding(1, PARENT, true, DENY_ALL)];
    let tenant_only = Stack::new(parent_policy.clone());
    let with_builtin = Stack::with_builtins(
        &[document("platform", DENY_ALL, &[WIDGET], &[])],
        parent_policy,
    );
    let decide = |stack: &Stack| {
        let service = std::sync::Arc::clone(&stack.service);
        async move {
            service
                .decide(&ctx_in(ISLAND), &request("create", ISLAND))
                .await
                .unwrap()
        }
    };
    assert!(matches!(
        decide(&tenant_only).await,
        EngineResult::Permit { .. }
    ));
    assert_eq!(decide(&with_builtin).await, builtin_denial());
}

#[tokio::test]
async fn tenant_policy_runs_when_no_builtin_applies_or_denies() {
    let stack = Stack::with_builtins(
        &[
            document("gadgets", DENY_ALL, &[GADGET], &[]),
            document("deletes", DENY_ALL, &[WIDGET], &["delete"]),
            document("silent", DENY_NONE, &[WIDGET], &[]),
        ],
        vec![
            widget_binding(1, CHILD, true, DENY_ALL),
            widget_binding(2, CHILD, false, DENY_ALL),
        ],
    );
    let verdict = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await
        .unwrap();
    assert_eq!(denials(&verdict), ["doc1"]);
    assert_eq!(shadows(&verdict), ["doc2"]);

    let verdict = stack
        .service
        .decide(&ctx_in(CHILD), &request("delete", CHILD))
        .await
        .unwrap();
    assert_eq!(verdict, builtin_denial());
}

#[tokio::test]
async fn a_failing_builtin_fails_closed() {
    let stack = Stack::with_builtins(
        &[document("broken", NOT_BOOLEAN, &[WIDGET], &[])],
        vec![widget_binding(1, CHILD, true, DENY_NONE)],
    );
    let result = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await;
    failed_with(&result, EngineFailureCondition::Internal);
    assert!(stack.bindings.asked.lock().is_empty());
}

#[tokio::test]
async fn an_anonymous_context_is_rejected_before_builtins_run() {
    let stack = Stack::with_builtins(
        &[document("platform", DENY_ALL, &[WIDGET], &[])],
        Vec::new(),
    );
    let result = stack
        .service
        .decide(&SecurityContext::anonymous(), &request("create", CHILD))
        .await;
    failed_with(&result, EngineFailureCondition::InvalidRequest);
}

#[tokio::test]
async fn an_evaluation_past_the_budget_is_a_timeout_not_an_internal_failure() {
    let short = Duration::from_millis(5);
    let tenant = Stack::with_evaluation_timeout(
        &[],
        vec![widget_binding(1, CHILD, true, &unbounded())],
        short,
    );
    let result = tenant
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await;
    failed_with(&result, EngineFailureCondition::Timeout);

    let builtin = Stack::with_evaluation_timeout(
        &[document("platform", &unbounded(), &[WIDGET], &[])],
        Vec::new(),
        short,
    );
    let result = builtin
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await;
    failed_with(&result, EngineFailureCondition::Timeout);
}

#[tokio::test]
async fn findings_past_the_admission_bound_are_capped_shadow_denials_first() {
    let over = MAX_ENGINE_FINDINGS as u128 + 6;
    let mut bindings: Vec<_> = (1..=over)
        .map(|n| widget_binding(n, CHILD, false, DENY_ALL))
        .collect();
    bindings.push(widget_binding(over + 1, CHILD, true, DENY_ALL));
    let stack = Stack::new(bindings);
    let result = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await
        .unwrap();
    assert_eq!(result.bounds_violation(), None);
    assert_eq!(denials(&result), [format!("doc{}", over + 1)]);
    assert_eq!(shadows(&result).len(), MAX_ENGINE_FINDINGS - 1);
}

#[tokio::test]
async fn a_store_outage_or_timeout_is_a_failure_never_a_permit() {
    let stack = Stack::new(vec![widget_binding(1, CHILD, true, DENY_NONE)]);
    for (fault, condition) in [
        (
            PortError::Unavailable("store down".to_owned()),
            EngineFailureCondition::Unavailable,
        ),
        (PortError::Timeout, EngineFailureCondition::Timeout),
    ] {
        *stack.bindings.fault.lock() = Some(fault);
        let result = stack
            .service
            .decide(&ctx_in(CHILD), &request("create", CHILD))
            .await;
        failed_with(&result, condition);
    }
}

#[tokio::test]
async fn active_content_that_no_longer_compiles_fails_closed() {
    let stack = Stack::new(vec![
        widget_binding(1, CHILD, true, DENY_NONE),
        widget_binding(2, CHILD, true, "package p\ndeny :="),
    ]);
    let result = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await;
    failed_with(&result, EngineFailureCondition::Internal);
    assert_eq!(stack.cache.len(), 1, "only the compiling version is cached");
}
