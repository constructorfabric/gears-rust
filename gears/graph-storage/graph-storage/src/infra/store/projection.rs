//! The payload-aware projection of the built-in store.
//!
//! The platform pager (`toolkit_db::odata::paginate_odata`) maps a field to a
//! column; a declared payload path is an extraction expression over the
//! `payload` column, which that binding has no slot for (DEVIATIONS D-104).
//! So when a projection names a payload path — in `$filter`, `$orderby` or
//! the order its cursor carries — the store renders the shared
//! [`Plan`](crate::domain::projection::Plan) itself: the same statement
//! shape the platform pager produces (filter, keyset predicate, order,
//! `limit + 1`, `CursorV1`), over expressions instead of columns. Column-only
//! projections keep taking the platform path, byte for byte as before.
//!
//! **No caller text reaches SQL.** A declared path's tokens are restricted to
//! `[A-Za-z0-9_.-]` at registration (`ontology::resolve_index_paths`), which
//! is what lets the path be rendered as a literal so the extraction matches
//! the containment form the GIN serves; every literal value is a bound
//! parameter.

use sea_orm::sea_query::{Expr, ExprTrait, Order};
use sea_orm::{Condition, Value};
use toolkit_odata::{CursorV1, SortDir};

use crate::domain::ontology::ScalarKind;
use crate::domain::projection::{CmpOp, FieldRef, OrderTerm, Plan, Predicate, Scalar, TextOp};
use crate::infra::storage::entity::node;

/// Marker prefixes of one cursor key: a present value and an absent one. A
/// payload path is nullable (the property may be missing on a row), and the
/// keyset has to say on which side of the nulls the page ended.
const PRESENT: &str = "v:";
const ABSENT: &str = "n:";

/// Tokens of a pointer below `/payload`, e.g. `["loc", "line"]`.
fn payload_tokens(pointer: &str) -> Vec<&str> {
    pointer
        .strip_prefix("/payload/")
        .map(|rest| rest.split('/').collect())
        .unwrap_or_default()
}

/// The `text[]` literal `'{a,b}'` for the `#>` / `#>>` operators.
fn path_literal(pointer: &str) -> String {
    format!("'{{{}}}'", payload_tokens(pointer).join(","))
}

/// The SQL expression that reads one field with its scalar semantics.
fn extraction(field: &FieldRef) -> Expr {
    match field {
        FieldRef::NodeKey => Expr::col(node::Column::NodeKey),
        FieldRef::Name => Expr::col(node::Column::Name),
        FieldRef::CreatedAt => Expr::col(node::Column::CreatedAt),
        FieldRef::UpdatedAt => Expr::col(node::Column::UpdatedAt),
        FieldRef::Payload { pointer, kind } => {
            let path = path_literal(pointer);
            match kind {
                // Text as stored; a date-time compares as its RFC 3339 text.
                ScalarKind::String | ScalarKind::DateTime => {
                    Expr::cust(format!("(payload #>> {path})"))
                }
                // Typed extraction guarded by the JSON type, so a row whose
                // payload disagrees with the schema reads as NULL rather than
                // failing the statement.
                ScalarKind::Number | ScalarKind::Integer => Expr::cust(format!(
                    "(CASE WHEN jsonb_typeof(payload #> {path}) = 'number' \
                     THEN (payload #> {path})::numeric END)"
                )),
                ScalarKind::Boolean => Expr::cust(format!(
                    "(CASE WHEN jsonb_typeof(payload #> {path}) = 'boolean' \
                     THEN (payload #> {path})::boolean END)"
                )),
            }
        }
    }
}

/// The bound parameter a literal becomes, typed to compare with
/// [`extraction`] of the same field.
fn bound(field: &FieldRef, value: &Scalar) -> Expr {
    match (field, value) {
        (_, Scalar::Str(s)) | (FieldRef::Payload { .. }, Scalar::DateTime(s)) => {
            Expr::val(s.clone())
        }
        (_, Scalar::Num(n)) => {
            Expr::cust_with_values("($1)::numeric", [Value::String(Some(n.clone()))])
        }
        (_, Scalar::Bool(b)) => Expr::val(*b),
        (_, Scalar::DateTime(s)) => {
            Expr::cust_with_values("($1)::timestamptz", [Value::String(Some(s.clone()))])
        }
    }
}

/// Equality over a payload path in the containment form the GIN index
/// serves: `payload @> '{"a":{"b":<value>}}'`.
fn containment(pointer: &str, value: &Scalar) -> Option<Expr> {
    let leaf = match value {
        Scalar::Str(s) | Scalar::DateTime(s) => serde_json::Value::String(s.clone()),
        Scalar::Bool(b) => serde_json::Value::Bool(*b),
        // A number in containment form has to be a JSON number with the same
        // canonical text; `12.50` and `12.5` are equal as numeric but not as
        // containment operands, so numbers take the typed comparison instead.
        Scalar::Num(_) => return None,
    };
    let mut document = leaf;
    for token in payload_tokens(pointer).iter().rev() {
        document = serde_json::json!({ *token: document });
    }
    Some(Expr::cust_with_values(
        "(payload @> ($1)::jsonb)",
        [Value::String(Some(document.to_string()))],
    ))
}

fn compare(field: &FieldRef, op: CmpOp, value: &Scalar) -> Expr {
    if let (CmpOp::Eq, FieldRef::Payload { pointer, .. }) = (op, field)
        && let Some(contained) = containment(pointer, value)
    {
        return contained;
    }
    let lhs = extraction(field);
    let rhs = bound(field, value);
    match op {
        CmpOp::Eq => lhs.eq(rhs),
        CmpOp::Ne => lhs.ne(rhs),
        CmpOp::Gt => lhs.gt(rhs),
        CmpOp::Ge => lhs.gte(rhs),
        CmpOp::Lt => lhs.lt(rhs),
        CmpOp::Le => lhs.lte(rhs),
    }
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Render the filter.
#[must_use]
pub fn condition(predicate: &Predicate) -> Condition {
    match predicate {
        Predicate::Compare { field, op, value } => Condition::all().add(compare(field, *op, value)),
        Predicate::In { field, values } => {
            let mut any = Condition::any();
            for value in values {
                any = any.add(compare(field, CmpOp::Eq, value));
            }
            any
        }
        Predicate::Text { field, op, needle } => {
            let escaped = escape_like(needle);
            let pattern = match op {
                TextOp::Contains => format!("%{escaped}%"),
                TextOp::StartsWith => format!("{escaped}%"),
                TextOp::EndsWith => format!("%{escaped}"),
            };
            Condition::all().add(extraction(field).like(pattern))
        }
        Predicate::And(children) => children
            .iter()
            .fold(Condition::all(), |acc, child| acc.add(condition(child))),
        Predicate::Or(children) => children
            .iter()
            .fold(Condition::any(), |acc, child| acc.add(condition(child))),
        Predicate::Not(inner) => Condition::all().not().add(condition(inner)),
    }
}

fn is_nullable(field: &FieldRef) -> bool {
    field.is_payload()
}

/// The `ORDER BY` terms: for a nullable field, its null flag first so nulls
/// sort last in either direction (`PostgreSQL` puts them first under `DESC`
/// by default), then the value.
#[must_use]
pub fn order_terms(plan: &Plan) -> Vec<(Expr, Order)> {
    let mut out = Vec::new();
    for term in &plan.order {
        if is_nullable(&term.field) {
            out.push((extraction(&term.field).is_null(), Order::Asc));
        }
        let order = match term.dir {
            SortDir::Asc => Order::Asc,
            SortDir::Desc => Order::Desc,
        };
        out.push((extraction(&term.field), order));
    }
    out
}

/// The cursor keys of one row: one entry per order term, marked present or
/// absent, in the text form [`keyset`] parses back.
pub fn cursor_keys(model: &node::Model, plan: &Plan) -> Result<Vec<String>, String> {
    plan.order
        .iter()
        .map(|term| {
            let value = match &term.field {
                FieldRef::NodeKey => Some(model.node_key.clone()),
                FieldRef::Name => Some(model.name.clone()),
                FieldRef::CreatedAt => Some(rfc3339(model.created_at)?),
                FieldRef::UpdatedAt => Some(rfc3339(model.updated_at)?),
                FieldRef::Payload { pointer, kind } => {
                    let inner = pointer.strip_prefix("/payload").unwrap_or(pointer);
                    model
                        .payload
                        .pointer(inner)
                        .and_then(|v| scalar_text(v, *kind))
                }
            };
            Ok(match value {
                Some(text) => format!("{PRESENT}{text}"),
                None => ABSENT.to_owned(),
            })
        })
        .collect()
}

fn rfc3339(at: time::OffsetDateTime) -> Result<String, String> {
    at.format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| format!("cannot format a timestamp: {error}"))
}

/// The comparable text of a payload value under its declared kind; `None`
/// when the stored value is not of that kind (it then reads as NULL).
fn scalar_text(value: &serde_json::Value, kind: ScalarKind) -> Option<String> {
    match (kind, value) {
        (ScalarKind::String | ScalarKind::DateTime, serde_json::Value::String(s)) => {
            Some(s.clone())
        }
        (ScalarKind::Number | ScalarKind::Integer, serde_json::Value::Number(n)) => {
            Some(n.to_string())
        }
        (ScalarKind::Boolean, serde_json::Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

fn scalar_from_text(field: &FieldRef, text: &str) -> Result<Scalar, String> {
    Ok(match field.kind() {
        ScalarKind::String => Scalar::Str(text.to_owned()),
        ScalarKind::DateTime => Scalar::DateTime(text.to_owned()),
        ScalarKind::Number | ScalarKind::Integer => {
            if text.parse::<f64>().is_err() {
                return Err(format!("cursor key `{text}` is not a number"));
            }
            Scalar::Num(text.to_owned())
        }
        ScalarKind::Boolean => Scalar::Bool(
            text.parse::<bool>()
                .map_err(|_| format!("cursor key `{text}` is not a boolean"))?,
        ),
    })
}

/// The keyset predicate "rows after the cursor" for the plan's order.
///
/// For terms `t1, t2, …` with keys `k1, k2, …` (nulls last in either
/// direction): a present key admits `t1 after k1`, `t1 IS NULL`, or
/// `t1 = k1 AND (rest)`; an absent key admits `t1 IS NULL AND (rest)`.
/// Columns are `NOT NULL`, so their null arms are omitted.
pub fn keyset(plan: &Plan, cursor: &CursorV1) -> Result<Condition, String> {
    if cursor.k.len() != plan.order.len() {
        return Err("cursor keys do not match the order".to_owned());
    }
    after(&plan.order, &cursor.k)
}

fn after(terms: &[OrderTerm], keys: &[String]) -> Result<Condition, String> {
    let (Some(term), Some(key)) = (terms.first(), keys.first()) else {
        // Nothing left to compare on: no row is "after" an identical key.
        return Ok(Condition::all().add(Expr::cust("FALSE")));
    };
    let rest = after(&terms[1..], &keys[1..])?;
    let expr = extraction(&term.field);
    if let Some(text) = key.strip_prefix(PRESENT) {
        let value = scalar_from_text(&term.field, text)?;
        let strictly_after = match term.dir {
            SortDir::Asc => expr.clone().gt(bound(&term.field, &value)),
            SortDir::Desc => expr.clone().lt(bound(&term.field, &value)),
        };
        let mut any = Condition::any().add(strictly_after);
        if is_nullable(&term.field) {
            any = any.add(expr.clone().is_null());
        }
        any = any.add(
            Condition::all()
                .add(expr.eq(bound(&term.field, &value)))
                .add(rest),
        );
        Ok(any)
    } else if key == ABSENT {
        if !is_nullable(&term.field) {
            return Err("cursor marks a non-null field absent".to_owned());
        }
        Ok(Condition::all().add(expr.is_null()).add(rest))
    } else {
        Err("malformed cursor key".to_owned())
    }
}

/// The signed order tokens the cursor carries (`+payload/score,-node_key`).
#[must_use]
pub fn signed_tokens(plan: &Plan) -> String {
    plan.order
        .iter()
        .map(|term| {
            let sign = match term.dir {
                SortDir::Asc => '+',
                SortDir::Desc => '-',
            };
            format!("{sign}{}", term.field.odata_name())
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::sea_query::{PostgresQueryBuilder, Query};

    fn render(expr: Expr) -> String {
        Query::select()
            .expr(expr)
            .to_owned()
            .to_string(PostgresQueryBuilder)
    }

    #[test]
    fn a_nested_path_renders_as_a_text_array_literal() {
        assert_eq!(path_literal("/payload/loc/line"), "'{loc,line}'");
        let rendered = render(extraction(&FieldRef::Payload {
            pointer: "/payload/loc/line".into(),
            kind: ScalarKind::Integer,
        }));
        assert!(
            rendered.contains("(payload #> '{loc,line}')::numeric"),
            "{rendered}"
        );
        assert!(rendered.contains("jsonb_typeof"), "{rendered}");
    }

    #[test]
    fn string_equality_takes_the_containment_form_the_index_serves() {
        let rendered = render(compare(
            &FieldRef::Payload {
                pointer: "/payload/loc/file".into(),
                kind: ScalarKind::String,
            },
            CmpOp::Eq,
            &Scalar::Str("a.rs".into()),
        ));
        assert!(
            rendered.contains(r#"payload @> ('{"loc":{"file":"a.rs"}}')::jsonb"#),
            "{rendered}"
        );
    }

    #[test]
    fn numeric_comparison_binds_a_numeric_cast() {
        let rendered = render(compare(
            &FieldRef::Payload {
                pointer: "/payload/score".into(),
                kind: ScalarKind::Number,
            },
            CmpOp::Gt,
            &Scalar::Num("5".into()),
        ));
        assert!(rendered.contains("> (('5')::numeric)"), "{rendered}");
    }

    #[test]
    fn like_patterns_escape_their_metacharacters() {
        assert_eq!(escape_like("50%_a\\"), "50\\%\\_a\\\\");
    }

    #[test]
    fn a_keyset_after_a_present_key_admits_the_null_tail() {
        let plan = Plan {
            filter: None,
            order: vec![
                OrderTerm {
                    field: FieldRef::Payload {
                        pointer: "/payload/score".into(),
                        kind: ScalarKind::Number,
                    },
                    dir: SortDir::Desc,
                },
                OrderTerm {
                    field: FieldRef::NodeKey,
                    dir: SortDir::Asc,
                },
            ],
        };
        let cursor = CursorV1 {
            k: vec!["v:7".into(), "v:k".into()],
            o: SortDir::Asc,
            s: signed_tokens(&plan),
            f: None,
            d: "fwd".into(),
        };
        assert_eq!(cursor.s, "-payload/score,+node_key");
        let condition = keyset(&plan, &cursor).expect("keyset builds");
        let rendered = Query::select()
            .expr(Expr::val(1))
            .cond_where(condition)
            .to_owned()
            .to_string(PostgresQueryBuilder);
        assert!(rendered.contains("< (('7')::numeric)"), "{rendered}");
        assert!(rendered.contains("IS NULL"), "{rendered}");
        assert!(rendered.contains(r#""node_key" > 'k'"#), "{rendered}");

        let absent = CursorV1 {
            k: vec![ABSENT.into(), "v:k".into()],
            ..cursor
        };
        let rendered = Query::select()
            .expr(Expr::val(1))
            .cond_where(keyset(&plan, &absent).expect("keyset builds"))
            .to_owned()
            .to_string(PostgresQueryBuilder);
        assert!(!rendered.contains("('7')"), "{rendered}");
        assert!(rendered.contains("IS NULL"), "{rendered}");
    }
}
