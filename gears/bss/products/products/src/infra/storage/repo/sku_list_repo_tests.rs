//! The pager's field vocabulary: what each field admits and which fields order.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;

fn string(s: &str) -> ODataValue {
    ODataValue::String(s.to_owned())
}

#[test]
fn only_the_four_non_nullable_keys_order_and_only_the_two_nullable_fields_compare_with_null() {
    for field in SkuListField::FIELDS {
        let orderable = matches!(
            field,
            SkuListField::Id | SkuListField::Code | SkuListField::Name | SkuListField::UpdatedAt
        );
        assert_eq!(SkuListMapping::is_orderable(*field), orderable, "{field:?}");
        assert!(
            !(field.nullable() && orderable),
            "{field:?}: a nullable key breaks the cursor"
        );
        let nullable = matches!(
            field,
            SkuListField::CategoryId | SkuListField::PendingUnitId
        );
        assert_eq!(field.nullable(), nullable, "{field:?}");
        // The toolkit's parser holds the line: `null` on any other field is a type mismatch.
        for op in ["eq", "ne"] {
            let parsed = toolkit_odata::filter::parse_odata_filter::<SkuListField>(&format!(
                "{} {op} null",
                field.name()
            ));
            assert_eq!(parsed.is_ok(), nullable, "{field:?} {op} null: {parsed:?}");
        }
    }
}

/// The closed fields compare with one of their values; the text functions the served contract
/// publishes for every text field take any text on them too.
#[test]
fn the_closed_fields_compare_with_their_values_and_take_the_text_functions() {
    for (field, good, bad) in [
        (SkuListField::Lifecycle, "published", "retiring"),
        (SkuListField::Type, "one_time", "onetime"),
    ] {
        for op in [FilterOp::Eq, FilterOp::Ne, FilterOp::In] {
            assert!(matches!(
                SkuListMapping::map_value(field, op, &string(good)),
                Ok(ODataValue::String(v)) if v == good
            ));
            assert!(SkuListMapping::map_value(field, op, &string(bad)).is_err());
        }
        for op in [FilterOp::Contains, FilterOp::StartsWith, FilterOp::EndsWith] {
            assert!(SkuListMapping::map_value(field, op, &string(&good[..2])).is_ok());
            assert!(SkuListMapping::map_value(field, op, &string(bad)).is_ok());
        }
    }
    // The open text fields take any text.
    assert!(
        SkuListMapping::map_value(SkuListField::Code, FilterOp::Contains, &string("%")).is_ok()
    );
}

#[test]
fn updated_at_orders_and_never_filters() {
    let toolkit_odata::filter::FilterNode::Binary { value: at, .. } =
        toolkit_odata::filter::parse_odata_filter::<SkuListField>(
            "updated_at eq 2026-01-01T00:00:00Z",
        )
        .unwrap()
    else {
        panic!("a comparison parses to a binary node");
    };
    for op in [FilterOp::Eq, FilterOp::Ge, FilterOp::Lt] {
        assert!(SkuListMapping::map_value(SkuListField::UpdatedAt, op, &at).is_err());
    }
    assert!(SkuListMapping::is_orderable(SkuListField::UpdatedAt));
}

/// `q` folds both sides the same way and escapes its pattern, on both dialects, for each of the
/// five columns: `LOWER(col) LIKE LOWER(?) ESCAPE '\'` on `SQLite`, and on Postgres through the
/// ICU root collation, `lower(col COLLATE "und-x-icu") LIKE lower(? COLLATE "und-x-icu")`, so the
/// database's locale does not decide the fold.
#[test]
fn the_text_search_lowers_both_sides_and_escapes_on_both_dialects() {
    use sea_orm::{EntityTrait, QueryFilter, QueryTrait};
    let filter = SkuListFilter {
        text: Some("50%_Off".into()),
        ..SkuListFilter::default()
    };
    for backend in [DbBackend::Postgres, DbBackend::Sqlite] {
        let sql = sku::Entity::find()
            .filter(list_condition(Uuid::nil(), &filter, backend))
            .build(backend)
            .to_string();
        for column in ["code", "name", "unit", "usage_type_ref", "gl_code"] {
            let folded = if backend == DbBackend::Postgres {
                format!(
                    r#"(lower("products_sku"."{column}" COLLATE "und-x-icu")) LIKE (lower(E'%50\\%\\_Off%' COLLATE "und-x-icu"))"#
                )
            } else {
                format!(r#"LOWER("products_sku"."{column}") LIKE LOWER("#)
            };
            assert!(sql.contains(&folded), "{backend:?} {column}: {sql}");
        }
        assert_eq!(sql.matches(" ESCAPE ").count(), 5, "{backend:?}: {sql}");
        // The escaped pattern, as each dialect spells a backslash in a literal (Postgres
        // renders `E'…'`, doubling it).
        let escaped = if backend == DbBackend::Postgres {
            r"'%50\\%\\_Off%'"
        } else {
            r"'%50\%\_Off%'"
        };
        assert!(sql.contains(escaped), "{backend:?}: {sql}");
    }
}

/// P-D-212: a usage filter binds its whole id set as ONE value — a `uuid[]` on Postgres, a JSON
/// array read by `json_each` on `SQLite` — so the statement is the same whatever the set's size,
/// and `false` negates the membership.
#[test]
fn a_usage_filter_binds_its_whole_set_once_on_both_dialects() {
    use sea_orm::{EntityTrait, QueryFilter, QueryTrait};
    for backend in [DbBackend::Postgres, DbBackend::Sqlite] {
        let mut statements = Vec::new();
        for n in [1_usize, 10, 1000] {
            let ids: Vec<Uuid> = (0..n).map(|_| Uuid::new_v4()).collect();
            let filter = SkuListFilter {
                priced: Some(SetFilter {
                    member: true,
                    ids: ids.clone(),
                }),
                in_plan: Some(SetFilter { member: false, ids }),
                ..SkuListFilter::default()
            };
            let statement = sku::Entity::find()
                .filter(list_condition(Uuid::nil(), &filter, backend))
                .build(backend);
            let binds = statement.values.map_or(0, |v| v.0.len());
            let sql = statement.sql;
            // The tenant, and one value per usage filter.
            assert_eq!(binds, 3, "{backend:?} n={n}: {sql}");
            statements.push(sql);
        }
        assert!(
            statements.windows(2).all(|w| w[0] == w[1]),
            "{backend:?}: one statement whatever the size: {statements:#?}"
        );
        let sql = &statements[0];
        let membership = if backend == DbBackend::Postgres {
            r#""products_sku"."id" = ANY(CAST($"#
        } else {
            r#""products_sku"."id" IN (SELECT unhex("value") FROM json_each(?))"#
        };
        assert_eq!(sql.matches(membership).count(), 2, "{backend:?}: {sql}");
        assert!(sql.contains("NOT"), "{backend:?}: `false` negates: {sql}");
    }
}
