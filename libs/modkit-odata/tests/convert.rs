#![allow(clippy::unwrap_used, clippy::expect_used)]

mod tests {
    use modkit_odata::ast::*;
    use modkit_odata::parse_filter_string;

    #[test]
    fn converts_and_contains() {
        let dst = parse_filter_string("score ge 10 and contains(email,'@acme.com')")
            .unwrap()
            .into_expr();

        match dst {
            Expr::And(a, b) => {
                match *a {
                    Expr::Compare(l, op, r) => {
                        assert!(matches!(*l, Expr::Identifier(_)));
                        assert!(matches!(op, CompareOperator::Ge));
                        assert!(matches!(*r, Expr::Value(Value::Number(_))));
                    }
                    _ => panic!("left not compare"),
                }
                match *b {
                    Expr::Function(ref name, ref args) => {
                        assert_eq!(name.to_lowercase(), "contains");
                        assert!(matches!(args[0], Expr::Identifier(_)));
                        assert!(matches!(args[1], Expr::Value(Value::String(_))));
                    }
                    _ => panic!("right not function"),
                }
            }
            _ => panic!("not And()"),
        }
    }

    #[test]
    fn converts_in_uuid() {
        let dst = parse_filter_string(
            "id in (00000000-0000-0000-0000-000000000001, 00000000-0000-0000-0000-000000000002)",
        )
        .unwrap()
        .into_expr();
        if let Expr::In(lhs, list) = dst {
            assert!(matches!(*lhs, Expr::Identifier(_)));
            assert_eq!(list.len(), 2);
        } else {
            panic!("expected In()");
        }
    }
}
