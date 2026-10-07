// Created: 2026-09-29 by Virtuozzo International GmbH
//! The shared filter grammar: what it accepts, how it splits, what it refuses.

use toolkit_odata::ast::Expr;

use super::interpret;
use crate::domain::error::DomainError;

fn parse(text: &str) -> Expr {
    toolkit_odata::parse_filter_string(text)
        .expect("the fixture parses")
        .into_expr()
}

fn refusal(text: &str) -> String {
    match interpret(Some(&parse(text))) {
        Err(DomainError::Validation { field, message, .. }) => {
            assert_eq!(field, "$filter");
            message
        }
        other => panic!("{text}: expected a refusal, got {other:?}"),
    }
}

#[test]
fn no_filter_asks_for_nothing() {
    let filter = interpret(None).expect("empty");
    assert!(!filter.needs_review);
    assert!(filter.keys.is_none());
    assert!(filter.declarations.is_none());
}

#[test]
fn the_declaration_selectors_stay_in_the_remainder_and_the_switch_leaves_it() {
    let id = uuid::Uuid::new_v4();
    let filter = interpret(Some(&parse(&format!(
        "category_id eq {id} and needs_review eq true and key in ('a', 'b')"
    ))))
    .expect("accepted");
    assert!(filter.needs_review);
    assert_eq!(
        filter.keys.as_deref(),
        Some(&["a".to_owned(), "b".to_owned()][..])
    );
    // `needs_review` is not a declaration column: it is taken out, and the
    // rest is re-joined for the page query.
    let remainder = format!("{:?}", filter.declarations.expect("a remainder"));
    assert!(
        remainder.contains("category_id") && remainder.contains("key"),
        "{remainder}"
    );
    assert!(!remainder.contains("needs_review"), "{remainder}");

    let only_switch = interpret(Some(&parse("needs_review eq true"))).expect("accepted");
    assert!(only_switch.needs_review);
    assert!(only_switch.declarations.is_none());

    let one_key = interpret(Some(&parse("key eq 'a'"))).expect("accepted");
    assert_eq!(one_key.keys.as_deref(), Some(&["a".to_owned()][..]));
    assert!(one_key.declarations.is_some());
}

#[test]
fn what_lies_outside_the_grammar_is_refused_with_the_grammar_named() {
    assert!(refusal("needs_review eq false").contains("`needs_review eq false` is not a listing"),);
    assert!(
        refusal("tenant eq 'x'").contains("tenant"),
        "an unmapped field"
    );
    assert!(refusal("key ne 'a'").contains("only `category_id eq`"));
    let id = uuid::Uuid::new_v4();
    assert!(refusal(&format!("category_id in ({id})")).contains("`in` is supported on `key` only"));
    assert!(refusal("key eq 'a' or key eq 'b'").contains("joined by `and`"));
}
