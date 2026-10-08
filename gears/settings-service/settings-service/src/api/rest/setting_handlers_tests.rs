// Created: 2026-09-07 by Virtuozzo International GmbH
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
fn two_declaration_selecting_clauses_stay_one_conjunction() {
    // Both halves select declarations, so both have to reach the repository —
    // dropping either would widen the page past what the client asked for.
    let f = interpret(Some(&parse(
        "key eq 'a.v1~' and category_id eq 0f7a3a8a-3a67-4a6b-9a2d-2b3f6d6f1c11",
    )))
    .expect("accepted");
    assert!(!f.needs_review);
    assert_eq!(f.keys.as_deref(), Some(&["a.v1~".to_owned()][..]));
    assert!(
        matches!(f.declarations, Some(Expr::And(..))),
        "both clauses survive the split: {:?}",
        f.declarations
    );
}

#[test]
fn a_conjunction_the_split_absorbs_entirely_leaves_no_declaration_filter() {
    // `needs_review` is a switch, not a column. When it is the whole filter
    // there is nothing left to narrow the declarations query with, and passing
    // an empty conjunction down would be a filter on nothing.
    let f = interpret(Some(&parse(
        "needs_review eq true and needs_review eq true",
    )))
    .expect("accepted");
    assert!(f.needs_review);
    assert!(f.declarations.is_none());
}

#[test]
fn in_is_refused_on_any_field_but_key() {
    // `key in (...)` exists to ask about a named set and report each member's
    // own outcome. No other field has per-member outcomes to report.
    match interpret(Some(&parse(
        "category_id in (0f7a3a8a-3a67-4a6b-9a2d-2b3f6d6f1c11)",
    ))) {
        Err(DomainError::Validation { field, message, .. }) => {
            assert_eq!(field, "$filter");
            assert!(message.contains("`key` only"), "got `{message}`");
        }
        other => panic!("expected a refusal, got {other:?}"),
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
