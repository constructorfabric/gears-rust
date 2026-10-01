//! The decision path over in-memory fakes and the real Rego backend.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::Ordering;

use policy_engine_sdk::reason;
use toolkit_security::SecurityContext;

use super::test_support::{
    CHILD, DENY_ALL, DENY_DELETE, DENY_NONE, GADGET, ISLAND, NOT_BOOLEAN, PARENT, SIBLING, Stack,
    WIDGET, binding, ctx_in, document, request, request_for, widget_binding,
};
use super::{DecisionFailure, Verdict};
use crate::domain::ports::PortError;

fn denials(verdict: &Verdict) -> Vec<String> {
    match verdict {
        Verdict::Deny { denials, .. } => denials.iter().map(|d| d.document_name.clone()).collect(),
        Verdict::Permit { .. } => Vec::new(),
    }
}

fn shadows(verdict: &Verdict) -> Vec<String> {
    match verdict {
        Verdict::Permit { shadow_denials } | Verdict::Deny { shadow_denials, .. } => shadow_denials
            .iter()
            .map(|d| d.document_name.clone())
            .collect(),
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
    assert_eq!(
        verdict,
        Verdict::Permit {
            shadow_denials: Vec::new()
        }
    );
}

#[tokio::test]
async fn an_anonymous_context_is_an_invalid_request() {
    let stack = Stack::new(Vec::new());
    let result = stack
        .service
        .decide(&SecurityContext::anonymous(), &request("create", CHILD))
        .await;
    assert!(matches!(result, Err(DecisionFailure::InvalidRequest(_))));
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
        assert_eq!(
            verdict,
            Verdict::Deny {
                reason_code: reason::TENANT_BOUNDARY,
                denials: Vec::new(),
                shadow_denials: Vec::new()
            }
        );
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
        assert!(matches!(verdict, Verdict::Permit { .. }), "caller {caller}");
    }
    assert_eq!(
        stack.hierarchy.reach_calls.load(Ordering::SeqCst),
        1,
        "the resource tenant itself needs no reachability call"
    );
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
    assert_eq!(
        verdict,
        Verdict::Permit {
            shadow_denials: Vec::new()
        }
    );
    assert_eq!(stack.bindings.asked.lock().as_slice(), [vec![ISLAND]]);
}

#[tokio::test]
async fn documents_apply_by_resource_type_and_action() {
    let documents = vec![
        document("widget-any", DENY_ALL, &[WIDGET], &[]),
        document("gadget-only", DENY_ALL, &[GADGET], &[]),
        document(
            "wildcard",
            DENY_ALL,
            &["gts.cf.core.example.*"],
            &["create"],
        ),
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
    assert!(matches!(permitted, Verdict::Permit { .. }));
    let denied = stack
        .service
        .decide(&ctx, &request("delete", CHILD))
        .await
        .unwrap();
    assert!(matches!(
        denied,
        Verdict::Deny {
            reason_code: reason::POLICY_DENIED,
            ..
        }
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
    assert!(matches!(verdict, Verdict::Permit { .. }));
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
async fn an_evaluation_error_fails_closed_even_beside_a_permit() {
    let stack = Stack::new(vec![
        widget_binding(1, CHILD, true, DENY_NONE),
        widget_binding(2, CHILD, false, NOT_BOOLEAN),
    ]);
    let result = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await;
    assert!(matches!(result, Err(DecisionFailure::Internal(_))));
}

#[tokio::test]
async fn hierarchy_timeouts_and_outages_are_failures_not_denials() {
    let stack = Stack::new(vec![widget_binding(1, CHILD, true, DENY_ALL)]);
    *stack.hierarchy.fault.lock() = Some(PortError::Timeout);
    let result = stack
        .service
        .decide(&ctx_in(PARENT), &request("create", CHILD))
        .await;
    assert!(matches!(result, Err(DecisionFailure::Timeout(_))));

    *stack.hierarchy.fault.lock() = Some(PortError::Unavailable("down".to_owned()));
    let result = stack
        .service
        .decide(&ctx_in(CHILD), &request("create", CHILD))
        .await;
    assert!(matches!(result, Err(DecisionFailure::Unavailable(_))));
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
