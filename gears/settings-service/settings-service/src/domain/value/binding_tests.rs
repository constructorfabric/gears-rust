//! The review cursor's binding: stable for one listing, different for another.

use toolkit_odata::ast::{CompareOperator, Expr, Value};
use uuid::Uuid;

use super::review_binding;

fn key_eq(key: &str) -> Expr {
    Expr::Compare(
        Box::new(Expr::Identifier("key".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::String(key.to_owned()))),
    )
}

#[test]
fn the_binding_is_stable_for_one_target_and_filter_and_differs_across_both() {
    let t1 = Uuid::from_u128(1);
    let t2 = Uuid::from_u128(2);
    let f1 = key_eq("a");
    let f2 = key_eq("b");

    let bound = review_binding(t1, Some(&f1));
    assert_eq!(bound, review_binding(t1, Some(&key_eq("a"))));
    assert_eq!(bound.len(), 16);
    assert_ne!(bound, review_binding(t2, Some(&f1)), "another target");
    assert_ne!(bound, review_binding(t1, Some(&f2)), "another filter");
    assert_ne!(bound, review_binding(t1, None), "no filter");
    assert_eq!(review_binding(t1, None), review_binding(t1, None));
}
