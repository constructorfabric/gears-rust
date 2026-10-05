#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use toolkit_odata::ast::{CompareOperator, Expr, Value};

fn id(name: &str) -> Expr {
    Expr::Identifier(name.to_owned())
}
fn cmp(field: &str, op: CompareOperator, value: Value) -> Expr {
    Expr::Compare(Box::new(id(field)), op, Box::new(Expr::Value(value)))
}
fn archived(op: CompareOperator, value: bool) -> Expr {
    cmp(ARCHIVED, op, Value::Bool(value))
}
fn code() -> Expr {
    cmp("code", CompareOperator::Eq, Value::String("A".into()))
}

#[test]
fn no_term_keeps_the_default_and_the_whole_filter() {
    let (rest, kept) = take_archived(code()).unwrap();
    assert_eq!(kept, Archived::Hidden);
    assert!(matches!(rest, Some(Expr::Compare(..))));
    assert!(matches!(
        take_archived_opt(None).unwrap(),
        (None, Archived::Hidden)
    ));
}

#[test]
fn eq_and_ne_with_a_boolean_choose_the_rows() {
    for (op, value, kept) in [
        (CompareOperator::Eq, true, Archived::Only),
        (CompareOperator::Eq, false, Archived::Hidden),
        (CompareOperator::Ne, true, Archived::Hidden),
        (CompareOperator::Ne, false, Archived::Only),
    ] {
        let (rest, found) = take_archived(archived(op, value)).unwrap();
        assert!(rest.is_none());
        assert_eq!(found, kept, "{op:?} {value}");
    }
}

#[test]
fn a_term_joined_by_and_leaves_the_rest_on_either_side() {
    let left = Expr::And(
        Box::new(archived(CompareOperator::Eq, true)),
        Box::new(code()),
    );
    let right = Expr::And(
        Box::new(code()),
        Box::new(archived(CompareOperator::Eq, true)),
    );
    for expr in [left, right] {
        let (rest, kept) = take_archived(expr).unwrap();
        assert_eq!(kept, Archived::Only);
        assert!(matches!(rest, Some(Expr::Compare(..))));
    }
    let both = Expr::And(
        Box::new(archived(CompareOperator::Eq, true)),
        Box::new(archived(CompareOperator::Eq, false)),
    );
    assert_eq!(take_archived(both).unwrap().1, Archived::Neither);
}

#[test]
fn any_other_use_of_archived_is_refused() {
    let or = Expr::Or(
        Box::new(archived(CompareOperator::Eq, true)),
        Box::new(code()),
    );
    let not = Expr::Not(Box::new(archived(CompareOperator::Eq, true)));
    let text = cmp(ARCHIVED, CompareOperator::Eq, Value::String("yes".into()));
    let order = archived(CompareOperator::Gt, false);
    let within = Expr::In(Box::new(id(ARCHIVED)), vec![Expr::Value(Value::Bool(true))]);
    let function = Expr::Function("contains".into(), vec![id(ARCHIVED)]);
    for expr in [or, not, text, order, within, function] {
        assert_eq!(
            take_archived(expr.clone()).unwrap_err(),
            ARCHIVED_FILTER_REFUSED,
            "{expr:?}"
        );
    }
}
