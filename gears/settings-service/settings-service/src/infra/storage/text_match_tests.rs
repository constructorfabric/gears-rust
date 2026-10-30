//! What the text match renders per dialect: the operator, the projection,
//! the escape clause, and the pattern bound in place, never written into
//! the SQL.

use sea_orm::ColumnTrait as _;
use sea_orm::sea_query::{Expr, PostgresQueryBuilder, Query};
use uuid::Uuid;

use super::{Dialect, TextMatch};
use crate::domain::category::visibility::DomainVisibility;
use crate::domain::search::{Corpus, Needle};

#[test]
fn the_json_null_guard_on_the_schema_default_renders_for_each_dialect() {
    assert_eq!(
        Dialect::Postgres.not_json_null("setting_declarations.default_value"),
        "jsonb_typeof(setting_declarations.default_value) <> 'null'"
    );
    assert_eq!(
        Dialect::Sqlite.not_json_null("setting_declarations.default_value"),
        "json_type(setting_declarations.default_value) <> 'null'"
    );
}

#[test]
fn the_declaration_text_read_renders_ilike_over_the_indexed_expressions_on_postgres() {
    // The browse narrowing reads the batch's ids with this predicate; on
    // PostgreSQL every text is `ILIKE` over the expression its trigram index
    // is built on, the pattern bound, never written into the SQL.
    let needle = Needle::parse("nee_dle").expect("needle");
    let ids = [Uuid::new_v4(), Uuid::new_v4()];
    let (sql, values) = Query::select()
        .expr(Expr::val(1))
        .cond_where(
            sea_orm::Condition::all()
                .add(crate::infra::storage::entity::declaration::Column::Id.is_in(ids))
                .add(TextMatch::new(Dialect::Postgres).declaration_text_matches(
                    &needle,
                    Corpus::PublicAndPii,
                    &DomainVisibility::Unrestricted,
                )),
        )
        .build(PostgresQueryBuilder);
    let pattern = sea_orm::Value::from("%nee\\_dle%".to_owned());
    for column in [
        "setting_declarations.key",
        "setting_declarations.display_name",
        "setting_declarations.description",
        "categories.name",
        "(setting_declarations.default_value #>> '{}')",
    ] {
        // The placeholder each text's `ILIKE` names is the index of a bound
        // value, and that value is the pattern.
        let rendered = format!("{column} ILIKE $");
        let at = sql
            .find(&rendered)
            .unwrap_or_else(|| panic!("{column}: {sql}"));
        let digits: String = sql[at + rendered.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let index: usize = digits.parse().expect("a placeholder index");
        assert_eq!(
            values.0.get(index - 1),
            Some(&pattern),
            "{column}: ${index}"
        );
    }
    assert_eq!(sql.matches(" ESCAPE '\\'").count(), 5, "{sql}");
    assert!(
        sql.contains("jsonb_typeof(setting_declarations.default_value) <> 'null'"),
        "{sql}"
    );
    assert_eq!(
        values.0.iter().filter(|value| **value == pattern).count(),
        5,
        "the pattern is a bound value for each of the five texts"
    );
}
