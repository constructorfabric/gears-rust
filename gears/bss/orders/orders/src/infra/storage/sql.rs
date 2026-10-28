//! Query-builder expressions for the PostgreSQL functions the storage statements call.
//!
//! TOOLKIT-DB-002 keeps raw SQL text out of repositories: every function call is a SeaQuery
//! [`Func::cust`] whose arguments are bound values, never a formatted string (the OAGW registry
//! state repository builds its `datetime(...)` call the same way). The statements render the
//! same SQL as the `Expr::cust` text they replace; the tests below pin that rendering.
use sea_orm::sea_query::{Asterisk, Expr, Func};

/// `clock_timestamp()`: fresh database wall-clock time, not transaction-start `now()`, so every
/// recorded instant and cutoff comes from the database rather than a replica clock.
pub fn db_clock() -> Expr {
    Func::cust("clock_timestamp").into()
}

/// `COUNT(*)`: the aggregate that makes a secured clock or privilege projection return exactly
/// one row even when its scope matches nothing. Its value is never read.
pub fn row_anchor() -> Expr {
    Func::count(Expr::col(Asterisk)).into()
}

/// The inline constant `FALSE`, for a probe that needs [`row_anchor`]'s one row but none of the
/// table's rows: an ungrouped aggregate still returns one row, and PostgreSQL folds a constant
/// false qualifier into a one-time filter, so the table is not scanned. It is a constant, not a
/// bound parameter, so a cached generic plan folds it too.
pub fn no_rows() -> Expr {
    Expr::Constant(false.into())
}

/// `current_setting('transaction_isolation')`: the isolation level of the running transaction.
pub fn isolation() -> Expr {
    Func::cust("current_setting")
        .arg("transaction_isolation")
        .into()
}

/// `has_table_privilege(table, 'INSERT')` for the session role, with both arguments bound.
pub fn insert_privilege(table: &str) -> Expr {
    Func::cust("has_table_privilege")
        .arg(table)
        .arg("INSERT")
        .into()
}

/// `pg_backend_pid()`: the PostgreSQL backend serving this session (fault tests only).
#[cfg(test)]
pub fn backend_pid() -> Expr {
    Func::cust("pg_backend_pid").into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::sea_query::{Alias, ExprTrait, PostgresQueryBuilder, Query};

    /// One projection rendered as PostgreSQL text with the values inlined.
    fn render(expr: Expr) -> String {
        Query::select().expr(expr).to_string(PostgresQueryBuilder)
    }

    #[test]
    fn functions_render_the_sql_they_replace() {
        // The previous `Expr::cust` text, rendered the same way.
        let previous = |text: &str| render(Expr::cust(text.to_owned()));
        assert_eq!(render(db_clock()), previous("clock_timestamp()"));
        assert_eq!(
            render(isolation()),
            previous("current_setting('transaction_isolation')")
        );
        assert_eq!(
            render(insert_privilege("bss_orders__transition_audit")),
            previous("has_table_privilege('bss_orders__transition_audit', 'INSERT')")
        );
        assert_eq!(render(backend_pid()), previous("pg_backend_pid()"));
        // Only the keyword case differs from the previous `count(*)`.
        assert_eq!(render(row_anchor()), "SELECT COUNT(*)");
    }

    #[test]
    fn arguments_are_bound_and_false_is_inline() {
        let (sql, values) = Query::select()
            .expr(isolation())
            .expr(insert_privilege("t"))
            .from(Alias::new("t"))
            .and_where(Expr::col(Alias::new("expires_at")).lt(db_clock()))
            .and_where(no_rows())
            .build(PostgresQueryBuilder);
        assert_eq!(
            sql,
            r#"SELECT current_setting($1), has_table_privilege($2, $3) FROM "t" WHERE "expires_at" < clock_timestamp() AND FALSE"#
        );
        assert_eq!(values.0.len(), 3);
    }
}
