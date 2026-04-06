use super::*;
use crate::ast::{CompareOperator, Expr, Value};

#[test]
fn test_normalize_filter_consistency() {
    // Test that the same logical filter produces the same normalized string
    let expr1 = Expr::Compare(
        Box::new(Expr::Identifier("name".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::String("test".to_owned()))),
    );

    let expr2 = Expr::Compare(
        Box::new(Expr::Identifier("name".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::String("test".to_owned()))),
    );

    assert_eq!(
        normalize_filter_for_hash(&expr1),
        normalize_filter_for_hash(&expr2)
    );
}

#[test]
fn test_short_filter_hash_consistency() {
    let expr = Expr::Compare(
        Box::new(Expr::Identifier("id".to_owned())),
        CompareOperator::Gt,
        Box::new(Expr::Value(Value::Number(42.into()))),
    );

    let hash1 = short_filter_hash(Some(&expr));
    let hash2 = short_filter_hash(Some(&expr));

    assert_eq!(hash1, hash2);
    assert!(hash1.is_some());
    assert_eq!(hash1.as_ref().unwrap().len(), 16); // 8 bytes = 16 hex chars
}

#[test]
fn test_short_filter_hash_none() {
    assert_eq!(short_filter_hash(None), None);
}
