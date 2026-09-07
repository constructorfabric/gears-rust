// Created: 2026-09-07 by Constructor Tech
//! The browse filter's interpretation: what is accepted and what is refused.

use toolkit_odata::ast::Expr;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{gate_target, interpret};
use crate::domain::error::DomainError;
use crate::domain::resolution::ScopeTarget;
use crate::test_support::ResolutionHarness;

fn parse(raw: &str) -> Expr {
    toolkit_odata::parse_filter_string(raw)
        .expect("well-formed OData")
        .into_expr()
}

#[test]
fn needs_review_is_split_off_and_the_rest_selects_declarations() {
    let f = interpret(Some(&parse(
        "needs_review eq true and category_id eq 0f7a3a8a-3a67-4a6b-9a2d-2b3f6d6f1c11",
    )))
    .expect("accepted");
    assert!(f.needs_review);
    assert!(f.keys.is_none());
    assert!(matches!(f.declarations, Some(Expr::Compare(..))));

    let only = interpret(Some(&parse("needs_review eq true"))).expect("accepted");
    assert!(only.needs_review);
    assert!(only.declarations.is_none());
}

#[test]
fn a_key_set_is_remembered_for_per_key_outcomes() {
    let f = interpret(Some(&parse("key in ('a.v1~','b.v1~')"))).expect("accepted");
    assert_eq!(
        f.keys.as_deref(),
        Some(&["a.v1~".to_owned(), "b.v1~".to_owned()][..])
    );
    assert!(matches!(f.declarations, Some(Expr::In(..))));

    let one = interpret(Some(&parse("key eq 'a.v1~'"))).expect("accepted");
    assert_eq!(one.keys.as_deref(), Some(&["a.v1~".to_owned()][..]));
}

#[test]
fn an_unmapped_field_or_an_unsupported_operator_is_refused_not_ignored() {
    for raw in [
        "tenant eq 'x'",
        "needs_review eq false",
        "category_id ne 0f7a3a8a-3a67-4a6b-9a2d-2b3f6d6f1c11",
        "key eq 'a' or key eq 'b'",
        "contains(key, 'a')",
    ] {
        match interpret(Some(&parse(raw))) {
            Err(DomainError::Validation { field, .. }) => assert_eq!(field, "$filter", "{raw}"),
            other => panic!("{raw}: expected a refusal, got {other:?}"),
        }
    }
}

#[test]
fn no_filter_browses_everything() {
    let f = interpret(None).expect("accepted");
    assert!(!f.needs_review && f.keys.is_none() && f.declarations.is_none());
}

fn caller(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .build()
        .expect("context")
}

#[tokio::test]
async fn the_target_is_the_caller_or_a_reachable_descendant_and_nothing_else() {
    let h = ResolutionHarness::new().await;
    let t = &h.tree;

    // Omitted: the caller's own tenant; for the root that is platform scope.
    assert_eq!(
        gate_target(&h.resolver, &caller(t.root), None)
            .await
            .expect("own"),
        ScopeTarget::Platform
    );
    assert_eq!(
        gate_target(&h.resolver, &caller(t.a), None)
            .await
            .expect("own"),
        ScopeTarget::Tenant(t.a)
    );

    // A descendant is fine; a sibling, an ancestor and a standalone descendant are denied.
    assert_eq!(
        gate_target(&h.resolver, &caller(t.a), Some(t.b))
            .await
            .expect("descendant"),
        ScopeTarget::Tenant(t.b)
    );
    for (who, target) in [(t.a, t.c), (t.b, t.a), (t.a, t.s), (t.root, t.s)] {
        match gate_target(&h.resolver, &caller(who), Some(target)).await {
            Err(DomainError::Unauthorized { .. }) => {}
            other => panic!("{who} -> {target}: expected a denial, got {other:?}"),
        }
    }
}
