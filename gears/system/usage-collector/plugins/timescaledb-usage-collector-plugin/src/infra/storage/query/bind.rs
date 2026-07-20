//! Value binding: convert a `toolkit_odata` AST value into a storage-typed
//! bind, and apply that bind to a `sqlx` query.
//!
//! `ODataValue` (re-exported as `toolkit_odata::filter::ODataValue`, the
//! `toolkit_odata::ast::Value` enum) carries `chrono`/`bigdecimal` concrete
//! types; the `usage_records` columns are `time::OffsetDateTime` /
//! `rust_decimal::Decimal`. This module owns that one-time conversion so the
//! translation layer and the stores never touch the AST value types directly.

// Vendored TimescaleDB raw-SQL backend: `sqlx` is required infra (hypertable
// time-series, `time_bucket` aggregation, keyset pagination — see DESIGN.md). Tenant
// isolation is enforced by hand via parameterized `tenant_id` predicates and an
// allowlisted-identifier query builder (DESIGN.md §Injection-Safe Query Translation),
// not SecureConn/AccessScope.
#![allow(unknown_lints, de0706_no_direct_sqlx)]

use std::str::FromStr;

use rust_decimal::Decimal;
use time::OffsetDateTime;
use uuid::Uuid;

use toolkit_odata::filter::ODataValue;

/// A storage-typed value ready to be bound to a `PostgreSQL` placeholder.
///
/// Each variant maps 1:1 to a `sqlx` `.bind` target whose Rust type matches a
/// `usage_records` / `usage_type_catalog` column (`uuid`, `text`, `numeric`,
/// `timestamptz`, `boolean`).
#[derive(Debug, Clone)]
pub enum SqlBind {
    /// `uuid` column bind.
    Uuid(Uuid),
    /// `text` column bind.
    Str(String),
    /// `numeric` column bind.
    Decimal(Decimal),
    /// `timestamptz` column bind.
    DateTime(OffsetDateTime),
    /// `boolean` column bind.
    Bool(bool),
}

/// Convert an `OData` AST value (`chrono`/`bigdecimal`-typed) into a
/// storage-typed [`SqlBind`].
///
/// `chrono::DateTime<Utc>` is converted to `time::OffsetDateTime` via the
/// nanosecond unix timestamp, and `bigdecimal::BigDecimal` to
/// `rust_decimal::Decimal` via its string form.
///
/// # Errors
///
/// Returns an error string on `Null` / `Date` / `Time` values (none of the
/// `usage_records` filter columns are date-only or time-only) or when a
/// numeric / datetime value is out of the target type's range.
pub fn odata_value_to_bind(v: &ODataValue) -> Result<SqlBind, String> {
    match v {
        ODataValue::Uuid(u) => Ok(SqlBind::Uuid(*u)),
        ODataValue::String(s) => Ok(SqlBind::Str(s.clone())),
        ODataValue::Bool(b) => Ok(SqlBind::Bool(*b)),
        ODataValue::Number(n) => Decimal::from_str(&n.to_string())
            .map(SqlBind::Decimal)
            .map_err(|e| format!("numeric out of range: {e}")),
        ODataValue::DateTime(dt) => {
            let nanos = dt
                .timestamp_nanos_opt()
                .ok_or_else(|| "datetime out of range".to_owned())?;
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
                .map(SqlBind::DateTime)
                .map_err(|e| format!("datetime conversion: {e}"))
        }
        ODataValue::Null => Err("null filter value unsupported".to_owned()),
        ODataValue::Date(_) | ODataValue::Time(_) => {
            Err("date/time-only filter values unsupported".to_owned())
        }
    }
}

/// Apply a single [`SqlBind`] to a `sqlx` `QueryAs` builder, returning it with
/// the bind appended. Generic over the row type `O` so it serves every
/// `query_as::<_, Row>` paginator (records and catalog).
///
/// Binds borrow from `v` for the `'q` lifetime, matching `sqlx`'s
/// borrow-the-argument binding model.
pub fn bind_one<'q, O>(
    q: sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>,
    v: &'q SqlBind,
) -> sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments> {
    match v {
        SqlBind::Uuid(u) => q.bind(u),
        SqlBind::Str(s) => q.bind(s),
        SqlBind::Decimal(d) => q.bind(d),
        SqlBind::DateTime(t) => q.bind(t),
        SqlBind::Bool(b) => q.bind(b),
    }
}

/// Apply a single [`SqlBind`] to a plain `sqlx` [`Query`] builder (no row
/// type), returning it with the bind appended.
///
/// The row-typed twin of [`bind_one`]: the aggregation path reads result
/// columns positionally (via [`sqlx::Row::try_get`]) rather than decoding into
/// a row struct, so it runs `sqlx::query(...)` rather than `query_as`. `Query`
/// and `QueryAs` share the same `.bind` signature, hence this parallel match.
///
/// Binds borrow from `v` for the `'q` lifetime, matching `sqlx`'s
/// borrow-the-argument binding model.
///
/// [`Query`]: sqlx::query::Query
pub fn bind_one_query<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    v: &'q SqlBind,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match v {
        SqlBind::Uuid(u) => q.bind(u),
        SqlBind::Str(s) => q.bind(s),
        SqlBind::Decimal(d) => q.bind(d),
        SqlBind::DateTime(t) => q.bind(t),
        SqlBind::Bool(b) => q.bind(b),
    }
}
