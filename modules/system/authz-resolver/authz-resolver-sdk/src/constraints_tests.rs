use super::*;
use modkit_security::pep_properties;
use serde_json::json;

#[test]
fn constraint_serialization_roundtrip() {
    let constraint = Constraint {
        predicates: vec![
            Predicate::In(InPredicate {
                property: pep_properties::OWNER_TENANT_ID.to_owned(),
                values: vec![
                    json!("11111111-1111-1111-1111-111111111111"),
                    json!("22222222-2222-2222-2222-222222222222"),
                ],
            }),
            Predicate::Eq(EqPredicate {
                property: pep_properties::RESOURCE_ID.to_owned(),
                value: json!("33333333-3333-3333-3333-333333333333"),
            }),
        ],
    };

    let json_str = serde_json::to_string(&constraint).unwrap();
    let deserialized: Constraint = serde_json::from_str(&json_str).unwrap();
    assert_eq!(deserialized.predicates.len(), 2);
}

#[test]
fn predicate_tag_serialization() {
    let eq = Predicate::Eq(EqPredicate {
        property: pep_properties::RESOURCE_ID.to_owned(),
        value: json!("00000000-0000-0000-0000-000000000000"),
    });

    let json_str = serde_json::to_string(&eq).unwrap();
    assert!(json_str.contains(r#""op":"eq""#));

    let in_pred = Predicate::In(InPredicate {
        property: pep_properties::OWNER_TENANT_ID.to_owned(),
        values: vec![json!("00000000-0000-0000-0000-000000000000")],
    });

    let json_str = serde_json::to_string(&in_pred).unwrap();
    assert!(json_str.contains(r#""op":"in""#));
}
