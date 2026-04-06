use super::*;
use crate::ast::{CompareOperator, Value};
use crate::schema::FieldRef;

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum UserField {
    Id,
    Name,
    Email,
    Age,
}

struct UserSchema;

impl Schema for UserSchema {
    type Field = UserField;

    fn field_name(field: Self::Field) -> &'static str {
        match field {
            UserField::Id => "id",
            UserField::Name => "name",
            UserField::Email => "email",
            UserField::Age => "age",
        }
    }
}

const NAME: FieldRef<UserSchema, String> = FieldRef::new(UserField::Name);
const EMAIL: FieldRef<UserSchema, String> = FieldRef::new(UserField::Email);
const AGE: FieldRef<UserSchema, i32> = FieldRef::new(UserField::Age);
const ID: FieldRef<UserSchema, uuid::Uuid> = FieldRef::new(UserField::Id);

#[test]
fn test_field_name_mapping() {
    assert_eq!(NAME.name(), "name");
    assert_eq!(EMAIL.name(), "email");
    assert_eq!(AGE.name(), "age");
}

#[test]
fn test_simple_eq_filter() {
    let user_id = uuid::Uuid::nil();
    let query = QueryBuilder::<UserSchema>::new()
        .filter(ID.eq(user_id))
        .build();

    assert!(query.has_filter());
    assert!(query.filter_hash.is_some());
}

#[test]
fn test_string_contains() {
    let query = QueryBuilder::<UserSchema>::new()
        .filter(NAME.contains("john"))
        .build();

    assert!(query.has_filter());
    if let Some(filter) = query.filter() {
        if let Expr::Function(name, args) = filter {
            assert_eq!(name, "contains");
            assert_eq!(args.len(), 2);
        } else {
            panic!("Expected Function expression");
        }
    }
}

#[test]
fn test_string_startswith() {
    let query = QueryBuilder::<UserSchema>::new()
        .filter(NAME.startswith("jo"))
        .build();

    assert!(query.has_filter());
    if let Some(filter) = query.filter() {
        if let Expr::Function(name, _) = filter {
            assert_eq!(name, "startswith");
        } else {
            panic!("Expected Function expression");
        }
    }
}

#[test]
fn test_string_endswith() {
    let query = QueryBuilder::<UserSchema>::new()
        .filter(EMAIL.endswith("@example.com"))
        .build();

    assert!(query.has_filter());
    if let Some(filter) = query.filter() {
        if let Expr::Function(name, _) = filter {
            assert_eq!(name, "endswith");
        } else {
            panic!("Expected Function expression");
        }
    }
}

#[test]
fn test_comparison_operators() {
    let query = QueryBuilder::<UserSchema>::new().filter(AGE.gt(18)).build();
    assert!(query.has_filter());

    let query = QueryBuilder::<UserSchema>::new().filter(AGE.ge(18)).build();
    assert!(query.has_filter());

    let query = QueryBuilder::<UserSchema>::new().filter(AGE.lt(65)).build();
    assert!(query.has_filter());

    let query = QueryBuilder::<UserSchema>::new().filter(AGE.le(65)).build();
    assert!(query.has_filter());

    let query = QueryBuilder::<UserSchema>::new().filter(AGE.ne(0)).build();
    assert!(query.has_filter());
}

#[test]
fn test_and_combinator() {
    let user_id = uuid::Uuid::nil();
    let query = QueryBuilder::<UserSchema>::new()
        .filter(ID.eq(user_id).and(AGE.gt(18)))
        .build();

    assert!(query.has_filter());
    if let Some(filter) = query.filter() {
        if let Expr::And(_, _) = filter {
        } else {
            panic!("Expected And expression");
        }
    }
}

#[test]
fn test_or_combinator() {
    let query = QueryBuilder::<UserSchema>::new()
        .filter(AGE.lt(18).or(AGE.gt(65)))
        .build();

    assert!(query.has_filter());
    if let Some(filter) = query.filter() {
        if let Expr::Or(_, _) = filter {
        } else {
            panic!("Expected Or expression");
        }
    }
}

#[test]
fn test_not_combinator() {
    let query = QueryBuilder::<UserSchema>::new()
        .filter(NAME.contains("test").not())
        .build();

    assert!(query.has_filter());
    if let Some(filter) = query.filter() {
        if let Expr::Not(_) = filter {
        } else {
            panic!("Expected Not expression");
        }
    }
}

#[test]
fn test_complex_filter() {
    let user_id = uuid::Uuid::nil();
    let query = QueryBuilder::<UserSchema>::new()
        .filter(
            ID.eq(user_id)
                .and(NAME.contains("john"))
                .and(AGE.ge(18).and(AGE.le(65))),
        )
        .build();

    assert!(query.has_filter());
    assert!(query.filter_hash.is_some());
}

#[test]
fn test_order_by_single() {
    let query = QueryBuilder::<UserSchema>::new()
        .order_by(NAME, SortDir::Asc)
        .build();

    assert_eq!(query.order.0.len(), 1);
    assert_eq!(query.order.0[0].field, "name");
    assert_eq!(query.order.0[0].dir, SortDir::Asc);
}

#[test]
fn test_order_by_multiple() {
    let query = QueryBuilder::<UserSchema>::new()
        .order_by(NAME, SortDir::Asc)
        .order_by(AGE, SortDir::Desc)
        .build();

    assert_eq!(query.order.0.len(), 2);
    assert_eq!(query.order.0[0].field, "name");
    assert_eq!(query.order.0[0].dir, SortDir::Asc);
    assert_eq!(query.order.0[1].field, "age");
    assert_eq!(query.order.0[1].dir, SortDir::Desc);
}

#[test]
fn test_select_fields() {
    let query = QueryBuilder::<UserSchema>::new()
        .select([NAME, EMAIL])
        .build();

    assert!(query.has_select());
    let fields = query.selected_fields().unwrap();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0], "name");
    assert_eq!(fields[1], "email");
}

#[test]
fn test_select_fields_vec() {
    let query = QueryBuilder::<UserSchema>::new()
        .select(vec![NAME, EMAIL])
        .build();

    assert!(query.has_select());
    let fields = query.selected_fields().unwrap();
    assert_eq!(fields, &["name", "email"]);
}

#[test]
fn test_select_fields_legacy_slice_syntax() {
    let query = QueryBuilder::<UserSchema>::new()
        .select(&[&NAME, &EMAIL])
        .build();

    assert!(query.has_select());
    let fields = query.selected_fields().unwrap();
    assert_eq!(fields, &["name", "email"]);
}

#[test]
fn test_page_size() {
    let query = QueryBuilder::<UserSchema>::new().page_size(50).build();

    assert_eq!(query.limit, Some(50));
}

#[test]
fn test_full_query_build() {
    let user_id = uuid::Uuid::nil();
    let query = QueryBuilder::<UserSchema>::new()
        .filter(ID.eq(user_id).and(AGE.gt(18)))
        .order_by(NAME, SortDir::Asc)
        .select([NAME, EMAIL])
        .page_size(25)
        .build();

    assert!(query.has_filter());
    assert!(query.filter_hash.is_some());
    assert_eq!(query.order.0.len(), 1);
    assert!(query.has_select());
    assert_eq!(query.limit, Some(25));
}

#[test]
fn test_filter_hash_stability() {
    let user_id = uuid::Uuid::nil();

    let query1 = QueryBuilder::<UserSchema>::new()
        .filter(ID.eq(user_id))
        .build();

    let query2 = QueryBuilder::<UserSchema>::new()
        .filter(ID.eq(user_id))
        .build();

    assert_eq!(query1.filter_hash, query2.filter_hash);
    assert!(query1.filter_hash.is_some());
}

#[test]
fn test_filter_hash_different_for_different_filters() {
    let query1 = QueryBuilder::<UserSchema>::new()
        .filter(NAME.eq("alice"))
        .build();

    let query2 = QueryBuilder::<UserSchema>::new().filter(AGE.gt(18)).build();

    assert_ne!(query1.filter_hash, query2.filter_hash);
}

#[test]
fn test_no_filter_no_hash() {
    let query = QueryBuilder::<UserSchema>::new()
        .order_by(NAME, SortDir::Asc)
        .build();

    assert!(!query.has_filter());
    assert!(query.filter_hash.is_none());
}

#[test]
fn test_empty_query() {
    let query = QueryBuilder::<UserSchema>::new().build();

    assert!(!query.has_filter());
    assert!(query.filter_hash.is_none());
    assert!(query.order.is_empty());
    assert!(!query.has_select());
    assert_eq!(query.limit, None);
}

#[test]
fn test_normalized_filter_consistency() {
    use crate::pagination::normalize_filter_for_hash;

    let expr1 = NAME.eq("test");
    let expr2 = NAME.eq("test");

    let norm1 = normalize_filter_for_hash(&expr1);
    let norm2 = normalize_filter_for_hash(&expr2);

    assert_eq!(norm1, norm2);
}

#[test]
fn test_is_null() {
    let query = QueryBuilder::<UserSchema>::new()
        .filter(NAME.is_null())
        .build();

    assert!(query.has_filter());
    if let Some(filter) = query.filter() {
        if let Expr::Compare(_, op, value) = filter {
            assert_eq!(*op, CompareOperator::Eq);
            if let Expr::Value(Value::Null) = **value {
            } else {
                panic!("Expected Value::Null");
            }
        } else {
            panic!("Expected Compare expression");
        }
    }
}

#[test]
fn test_is_not_null() {
    let query = QueryBuilder::<UserSchema>::new()
        .filter(EMAIL.is_not_null())
        .build();

    assert!(query.has_filter());
    if let Some(filter) = query.filter() {
        if let Expr::Compare(_, op, value) = filter {
            assert_eq!(*op, CompareOperator::Ne);
            if let Expr::Value(Value::Null) = **value {
            } else {
                panic!("Expected Value::Null");
            }
        } else {
            panic!("Expected Compare expression");
        }
    }
}

#[test]
fn test_chrono_datetime_conversion() {
    use chrono::Utc;

    const CREATED_AT: FieldRef<UserSchema, chrono::DateTime<Utc>> = FieldRef::new(UserField::Age);

    let now = Utc::now();
    let query = QueryBuilder::<UserSchema>::new()
        .filter(CREATED_AT.eq(now))
        .build();

    assert!(query.has_filter());
}

#[test]
fn test_chrono_naive_date_conversion() {
    use chrono::NaiveDate;

    const DATE_FIELD: FieldRef<UserSchema, NaiveDate> = FieldRef::new(UserField::Age);

    let date = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    let query = QueryBuilder::<UserSchema>::new()
        .filter(DATE_FIELD.eq(date))
        .build();

    assert!(query.has_filter());
}

#[test]
fn test_chrono_naive_time_conversion() {
    use chrono::NaiveTime;

    const TIME_FIELD: FieldRef<UserSchema, NaiveTime> = FieldRef::new(UserField::Age);

    let time = NaiveTime::from_hms_opt(12, 30, 0).unwrap();
    let query = QueryBuilder::<UserSchema>::new()
        .filter(TIME_FIELD.eq(time))
        .build();

    assert!(query.has_filter());
}
