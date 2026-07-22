//! Injection-safe filter translation: a validated `FilterNode<F>` becomes a
//! parameterized `PostgreSQL` `WHERE` fragment plus an ordered bind list.
//!
//! Identifiers come only from the closed allowlists ([`record_column`] /
//! [`usage_type_column`]); values are always bound (`$N`) via
//! [`crate::infra::storage::query::bind::odata_value_to_bind`].
//!
//! # Verified `toolkit-odata` / SDK API (Task E1)
//!
//! - Converter: `toolkit_odata::filter::convert_expr_to_filter_node::<F>(expr:
//!   &toolkit_odata::ast::Expr) -> Result<FilterNode<F>, FilterError>` — takes
//!   `&Expr` (the AST node, e.g. from `ODataQuery::filter()`), NOT
//!   `&ODataQuery`. (`parse_odata_filter::<F>(&str)` is the string entry point.)
//! - `FilterNode<F>` variants: `Binary { field: F, op: FilterOp, value:
//!   ODataValue }`, `InList { field: F, values: Vec<ODataValue> }`, `Composite
//!   { op: FilterOp, children: Vec<FilterNode<F>> }`, `Not(Box<FilterNode<F>>)`.
//! - `FilterOp` variants: `Eq, Ne, Gt, Ge, Lt, Le, In, Contains, StartsWith,
//!   EndsWith, And, Or`.
//! - `FilterField` (`toolkit_odata::filter::FilterField`): `const FIELDS:
//!   &'static [Self]`, `fn name(&self) -> &'static str`, `fn kind(&self) ->
//!   FieldKind`, `fn from_name(name: &str) -> Option<Self>`. `name()` returns
//!   the macro field's snake-case name — for `UsageRecordFilterField` those are
//!   exactly `"id"`, `"created_at"`, `"tenant_id"`, `"resource_id"`,
//!   `"resource_type"`, `"subject_id"`, `"subject_type"`, `"corrects_id"`,
//!   `"status"`; for `UsageTypeFilterField`, `"gts_id"` and `"kind"`. The
//!   identity column allowlists below rely on that.
//! - `ODataValue` path: `toolkit_odata::filter::ODataValue` is a `pub use` of
//!   `toolkit_odata::ast::Value`. Variants: `Null`, `Bool(bool)`,
//!   `Number(bigdecimal::BigDecimal)`, `Uuid(uuid::Uuid)`,
//!   `DateTime(chrono::DateTime<chrono::Utc>)`, `Date(chrono::NaiveDate)`,
//!   `Time(chrono::NaiveTime)`, `String(String)`.
//! - `UsageRecordFilterField` / `UsageTypeFilterField` are SDK re-exports
//!   (`UsageRecordQueryFilterField` / `UsageTypeQueryFilterField`,
//!   `#[derive(ODataFilterable)]`-generated). Tests build them via
//!   `<UsageRecordFilterField as FilterField>::from_name("status")`.
//! - `UsageTypeGtsId`: `new(impl Into<String>) -> Result<Self,
//!   UsageCollectorError>` (validated); reads back via `AsRef<str>`
//!   (`as_ref()`). `ResourceRef::new(resource_id, resource_type) -> Result<_,
//!   _>`; `SubjectRef::new(subject_id, Option<subject_type>) -> Result<_, _>`;
//!   `MetadataKey::new(impl Into<String>) -> Result<_, _>`;
//!   `IdempotencyKey::new(impl Into<String>) -> Result<_, _>`.

use toolkit_odata::filter::{FilterField, FilterNode, FilterOp};

pub use super::bind::{SqlBind, bind_one, bind_one_query, odata_value_to_bind};
pub use toolkit_odata::filter::ODataValue;

/// Closed allowlist mapping a `usage_records` filter-field name to its column.
///
/// The map is the identity (field name == column name); the closed `match` is
/// the security boundary — only these nine identifiers can ever reach the SQL
/// string. `gts_id` is intentionally absent: it is a typed parameter on the
/// SPI, not a `$filter` field.
#[must_use]
pub fn record_column(field_name: &str) -> Option<&'static str> {
    match field_name {
        "id" => Some("id"),
        "created_at" => Some("created_at"),
        "tenant_id" => Some("tenant_id"),
        "resource_id" => Some("resource_id"),
        "resource_type" => Some("resource_type"),
        "subject_id" => Some("subject_id"),
        "subject_type" => Some("subject_type"),
        "corrects_id" => Some("corrects_id"),
        "status" => Some("status"),
        _ => None,
    }
}

/// Closed allowlist mapping a `usage_type_catalog` filter-field name to its
/// column. Identity map over the two filterable catalog fields.
#[must_use]
pub fn usage_type_column(field_name: &str) -> Option<&'static str> {
    match field_name {
        "gts_id" => Some("gts_id"),
        "kind" => Some("kind"),
        _ => None,
    }
}

/// Bind accumulator + placeholder counter for a single SQL statement.
///
/// `next` is the next `$N` index to emit; `binds` is the ordered list of
/// values to apply (via [`bind_one`]) in `$1, $2, …` order. Callers seed the
/// start index so a filter fragment can follow leading binds (e.g. a `gts_id`
/// bound at `$1`).
pub struct SqlCtx {
    next: usize,
    /// Accumulated binds in placeholder order. Crate-visible: read only by the
    /// in-crate stores (record/catalog) and the query tests — never by an
    /// external consumer.
    pub(crate) binds: Vec<SqlBind>,
}

impl SqlCtx {
    /// Create a context whose first emitted placeholder is `$start`.
    #[must_use]
    pub fn new(start: usize) -> Self {
        Self {
            next: start,
            binds: Vec::new(),
        }
    }

    /// Append a bind and return the `$N` index it occupies. Crate-visible so
    /// the keyset helper accumulates binds in the same ordered context.
    pub(crate) fn push(&mut self, b: SqlBind) -> usize {
        let n = self.next;
        self.next += 1;
        self.binds.push(b);
        n
    }
}

/// Map a comparison [`FilterOp`] to its SQL operator.
///
/// # Errors
///
/// Returns an error string for non-comparison operators (`In` / `Contains` /
/// `StartsWith` / `EndsWith` / `And` / `Or`): the SPI filter fields are
/// exact-match, so `LIKE`-family operators are out of scope, and the composite
/// / membership operators are handled structurally by the translators.
fn op_sql(op: FilterOp) -> Result<&'static str, String> {
    match op {
        FilterOp::Eq => Ok("="),
        FilterOp::Ne => Ok("<>"),
        FilterOp::Gt => Ok(">"),
        FilterOp::Ge => Ok(">="),
        FilterOp::Lt => Ok("<"),
        FilterOp::Le => Ok("<="),
        other => Err(format!("unsupported operator: {other:?}")),
    }
}

/// Translate a `usage_records` filter node into a parameterized `WHERE`
/// fragment, pushing each value onto `ctx` as a bind.
///
/// Identifiers resolve through [`record_column`]; an unmapped field is an
/// error (never interpolated). Values resolve through
/// [`odata_value_to_bind`].
///
/// # Errors
///
/// Returns an error string when a field is not on the allowlist, an operator is
/// unsupported, a composite carries a non-`And`/`Or` operator, or a value
/// cannot be converted to a bind.
pub fn translate_record_filter<F: FilterField>(
    node: &FilterNode<F>,
    ctx: &mut SqlCtx,
) -> Result<String, String> {
    translate_filter(node, ctx, record_column)
}

/// Translate a `usage_type_catalog` filter node into a parameterized `WHERE`
/// fragment. Identical to [`translate_record_filter`] but resolves identifiers
/// through [`usage_type_column`].
///
/// # Errors
///
/// Same conditions as [`translate_record_filter`].
pub fn translate_usage_type_filter<F: FilterField>(
    node: &FilterNode<F>,
    ctx: &mut SqlCtx,
) -> Result<String, String> {
    translate_filter(node, ctx, usage_type_column)
}

/// Shared recursive walker parameterized over the column allowlist.
fn translate_filter<F: FilterField>(
    node: &FilterNode<F>,
    ctx: &mut SqlCtx,
    col: fn(&str) -> Option<&'static str>,
) -> Result<String, String> {
    match node {
        FilterNode::Binary { field, op, value } => {
            let column = col(field.name())
                .ok_or_else(|| format!("field not allowlisted: {}", field.name()))?;
            let operator = op_sql(*op)?;
            let n = ctx.push(odata_value_to_bind(value)?);
            Ok(format!("{column} {operator} ${n}"))
        }
        FilterNode::InList { field, values } => {
            let column = col(field.name())
                .ok_or_else(|| format!("field not allowlisted: {}", field.name()))?;
            if values.is_empty() {
                return Err("IN list must not be empty".to_owned());
            }
            let placeholders = values
                .iter()
                .map(|v| Ok(format!("${}", ctx.push(odata_value_to_bind(v)?))))
                .collect::<Result<Vec<_>, String>>()?;
            Ok(format!("{column} IN ({})", placeholders.join(", ")))
        }
        FilterNode::Composite { op, children } => {
            let joiner = match op {
                FilterOp::And => " AND ",
                FilterOp::Or => " OR ",
                other => return Err(format!("invalid composite operator: {other:?}")),
            };
            let parts = children
                .iter()
                .map(|child| translate_filter(child, ctx, col))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("({})", parts.join(joiner)))
        }
        FilterNode::Not(inner) => Ok(format!("NOT ({})", translate_filter(inner, ctx, col)?)),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "translate_tests.rs"]
mod translate_tests;
