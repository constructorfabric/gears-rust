//! Planning of a tabular projection over payload attributes.
//!
//! The platform `OData` binding fixes the filterable fields at compile time
//! (`FilterField::FIELDS`), while a payload path belongs to a tenant's
//! ontology: it is admitted by the `index` trait of the types the projection
//! selects, and its scalar kind comes from their schemas. This module is the
//! part both stores share — pure logic, no I/O: it reads the parsed query,
//! resolves every identifier to a column or an admitted payload path with its
//! kind, checks literal types, and hands each store one plan to execute. The
//! `PostgreSQL` store renders it to SQL, the fake evaluates it in memory, so
//! the admissibility rules cannot drift between them (ADR-0003).

use std::collections::BTreeMap;

use toolkit_odata::ODataQuery;
use toolkit_odata::SortDir;
use toolkit_odata::ast::{CompareOperator, Expr, Value as Literal};

use crate::domain::ontology::ScalarKind;

/// The four column fields the projection has always exposed.
pub const COLUMN_FIELDS: [&str; 4] = ["node_key", "name", "created_at", "updated_at"];

/// The field a term of the plan reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldRef {
    NodeKey,
    Name,
    CreatedAt,
    UpdatedAt,
    /// A declared payload path: the JSON pointer from the document root and
    /// the kind its schema gives it.
    Payload {
        pointer: String,
        kind: ScalarKind,
    },
}

impl FieldRef {
    /// The `OData` spelling of the field (`payload/severity`).
    #[must_use]
    pub fn odata_name(&self) -> String {
        match self {
            Self::NodeKey => "node_key".to_owned(),
            Self::Name => "name".to_owned(),
            Self::CreatedAt => "created_at".to_owned(),
            Self::UpdatedAt => "updated_at".to_owned(),
            Self::Payload { pointer, .. } => pointer.trim_start_matches('/').to_owned(),
        }
    }

    /// What kind of literal the field compares with.
    #[must_use]
    pub fn kind(&self) -> ScalarKind {
        match self {
            Self::NodeKey | Self::Name => ScalarKind::String,
            Self::CreatedAt | Self::UpdatedAt => ScalarKind::DateTime,
            Self::Payload { kind, .. } => *kind,
        }
    }

    #[must_use]
    pub fn is_payload(&self) -> bool {
        matches!(self, Self::Payload { .. })
    }
}

/// A comparison literal, already checked against the field's kind.
#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    Str(String),
    /// Canonical decimal text (`BigDecimal` display), so no store is tied to
    /// one numeric crate; the store binds it with a numeric cast.
    Num(String),
    Bool(bool),
    /// RFC 3339 text in UTC.
    DateTime(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextOp {
    Contains,
    StartsWith,
    EndsWith,
}

/// The filter, resolved.
#[derive(Clone, Debug, PartialEq)]
pub enum Predicate {
    Compare {
        field: FieldRef,
        op: CmpOp,
        value: Scalar,
    },
    In {
        field: FieldRef,
        values: Vec<Scalar>,
    },
    Text {
        field: FieldRef,
        op: TextOp,
        needle: String,
    },
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
    Not(Box<Predicate>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderTerm {
    pub field: FieldRef,
    pub dir: SortDir,
}

/// One projection, resolved and admitted.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub filter: Option<Predicate>,
    /// The effective order, always ending in the `node_key` tiebreaker.
    pub order: Vec<OrderTerm>,
}

impl Plan {
    #[must_use]
    pub fn references_payload(&self) -> bool {
        self.order.iter().any(|t| t.field.is_payload())
            || self.filter.as_ref().is_some_and(predicate_reads_payload)
    }
}

fn predicate_reads_payload(predicate: &Predicate) -> bool {
    match predicate {
        Predicate::Compare { field, .. }
        | Predicate::In { field, .. }
        | Predicate::Text { field, .. } => field.is_payload(),
        Predicate::And(children) | Predicate::Or(children) => {
            children.iter().any(predicate_reads_payload)
        }
        Predicate::Not(inner) => predicate_reads_payload(inner),
    }
}

/// Why a query could not be planned. Always a caller error — the message
/// names the offending identifier and the admitted alternatives, which is
/// what makes it actionable (PRD § fr-tabular-projection).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PlanError(pub String);

/// Does the query name any payload path at all — in `$filter`, `$orderby`, or
/// the order a cursor carries? Decides whether a store may take its
/// column-only fast path.
#[must_use]
pub fn mentions_payload(query: &ODataQuery) -> bool {
    if query.order.0.iter().any(|k| is_payload_name(&k.field)) {
        return true;
    }
    if let Some(cursor) = &query.cursor
        && cursor
            .s
            .split(',')
            .any(|token| is_payload_name(token.trim_start_matches(['+', '-'])))
    {
        return true;
    }
    query.filter.as_deref().is_some_and(expr_mentions_payload)
}

fn is_payload_name(name: &str) -> bool {
    name.starts_with("payload/")
}

fn expr_mentions_payload(expr: &Expr) -> bool {
    match expr {
        Expr::Identifier(name) => is_payload_name(name),
        Expr::Value(_) => false,
        Expr::Not(inner) => expr_mentions_payload(inner),
        Expr::And(a, b) | Expr::Or(a, b) | Expr::Compare(a, _, b) => {
            expr_mentions_payload(a) || expr_mentions_payload(b)
        }
        Expr::In(a, list) => expr_mentions_payload(a) || list.iter().any(expr_mentions_payload),
        Expr::Function(_, args) => args.iter().any(expr_mentions_payload),
    }
}

/// The payload paths a projection over `type_kinds` may name: a path counts
/// only when **every** selected type declares it, with one kind. A filter
/// over a path some selected type lacks would silently exclude that type's
/// rows, which is a wrong answer rather than a narrower one.
#[must_use]
pub fn admitted_paths(type_kinds: &[BTreeMap<String, ScalarKind>]) -> BTreeMap<String, ScalarKind> {
    let Some((first, rest)) = type_kinds.split_first() else {
        return BTreeMap::new();
    };
    first
        .iter()
        .filter(|(pointer, kind)| {
            rest.iter()
                .all(|other| other.get(*pointer).is_some_and(|k| k == *kind))
        })
        .map(|(pointer, kind)| (pointer.clone(), *kind))
        .collect()
}

/// Resolve the query against the admitted payload paths.
///
/// `admitted` is `None` when the projection carries no `type_pattern`: then
/// no payload path can be admitted, because there is no type set to read the
/// declarations from, and the error says so.
pub fn plan(
    query: &ODataQuery,
    admitted: Option<&BTreeMap<String, ScalarKind>>,
) -> Result<Plan, PlanError> {
    let resolver = Resolver { admitted };

    let filter = match query.filter.as_deref() {
        Some(expr) => Some(resolver.predicate(expr)?),
        None => None,
    };

    // A cursor carries the order it was minted under; a query with one may
    // not re-order (the platform parser already refuses `$orderby` next to
    // a cursor), so the cursor's tokens are the effective order.
    let mut order: Vec<OrderTerm> = Vec::new();
    if let Some(cursor) = &query.cursor {
        for token in cursor.s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            let (dir, name) = match token.as_bytes()[0] {
                b'-' => (SortDir::Desc, &token[1..]),
                b'+' => (SortDir::Asc, &token[1..]),
                _ => (SortDir::Asc, token),
            };
            order.push(OrderTerm {
                field: resolver.field(name)?,
                dir,
            });
        }
    } else {
        for key in &query.order.0 {
            order.push(OrderTerm {
                field: resolver.field(&key.field)?,
                dir: key.dir,
            });
        }
    }
    if !order.iter().any(|t| t.field == FieldRef::NodeKey) {
        order.push(OrderTerm {
            field: FieldRef::NodeKey,
            dir: SortDir::Asc,
        });
    }

    Ok(Plan { filter, order })
}

struct Resolver<'a> {
    admitted: Option<&'a BTreeMap<String, ScalarKind>>,
}

impl Resolver<'_> {
    fn field(&self, name: &str) -> Result<FieldRef, PlanError> {
        match name {
            "node_key" => return Ok(FieldRef::NodeKey),
            "name" => return Ok(FieldRef::Name),
            "created_at" => return Ok(FieldRef::CreatedAt),
            "updated_at" => return Ok(FieldRef::UpdatedAt),
            _ => {}
        }
        if !is_payload_name(name) {
            return Err(PlanError(format!(
                "unknown field `{name}`; the projection accepts {}{}",
                COLUMN_FIELDS.join(", "),
                self.alternatives()
            )));
        }
        let pointer = format!("/{name}");
        let Some(admitted) = self.admitted else {
            return Err(PlanError(format!(
                "`{name}` is a payload path; filtering or ordering by payload needs a \
                 `type_pattern`, because the admitted paths are read from the selected \
                 types' `index` trait"
            )));
        };
        match admitted.get(&pointer) {
            Some(kind) => Ok(FieldRef::Payload {
                pointer,
                kind: *kind,
            }),
            None => Err(PlanError(format!(
                "`{name}` is not declared in the `index` trait of every selected type; the \
                 projection accepts {}{}",
                COLUMN_FIELDS.join(", "),
                self.alternatives()
            ))),
        }
    }

    fn alternatives(&self) -> String {
        match self.admitted {
            Some(admitted) if !admitted.is_empty() => {
                let names: Vec<String> = admitted
                    .keys()
                    .map(|p| p.trim_start_matches('/').to_owned())
                    .collect();
                format!(" and the declared payload paths {}", names.join(", "))
            }
            Some(_) => " (the selected types declare no payload paths)".to_owned(),
            None => String::new(),
        }
    }

    fn predicate(&self, expr: &Expr) -> Result<Predicate, PlanError> {
        match expr {
            Expr::And(a, b) => Ok(Predicate::And(vec![self.predicate(a)?, self.predicate(b)?])),
            Expr::Or(a, b) => Ok(Predicate::Or(vec![self.predicate(a)?, self.predicate(b)?])),
            Expr::Not(inner) => Ok(Predicate::Not(Box::new(self.predicate(inner)?))),
            Expr::Compare(left, op, right) => {
                let (name, literal) = match (&**left, &**right) {
                    (Expr::Identifier(name), Expr::Value(value)) => (name, value),
                    (Expr::Identifier(_), Expr::Identifier(_)) => {
                        return Err(PlanError(
                            "a comparison between two fields is not supported".to_owned(),
                        ));
                    }
                    _ => {
                        return Err(PlanError(
                            "a comparison must be between a field and a literal".to_owned(),
                        ));
                    }
                };
                let field = self.field(name)?;
                let value = coerce(&field, literal)?;
                let op = match op {
                    CompareOperator::Eq => CmpOp::Eq,
                    CompareOperator::Ne => CmpOp::Ne,
                    CompareOperator::Gt => CmpOp::Gt,
                    CompareOperator::Ge => CmpOp::Ge,
                    CompareOperator::Lt => CmpOp::Lt,
                    CompareOperator::Le => CmpOp::Le,
                };
                Ok(Predicate::Compare { field, op, value })
            }
            Expr::In(left, list) => {
                let Expr::Identifier(name) = &**left else {
                    return Err(PlanError("`in` needs a field on its left".to_owned()));
                };
                let field = self.field(name)?;
                let mut values = Vec::with_capacity(list.len());
                for item in list {
                    let Expr::Value(literal) = item else {
                        return Err(PlanError("`in` accepts literals only".to_owned()));
                    };
                    values.push(coerce(&field, literal)?);
                }
                Ok(Predicate::In { field, values })
            }
            Expr::Function(name, args) => {
                let op = match name.to_ascii_lowercase().as_str() {
                    "contains" => TextOp::Contains,
                    "startswith" => TextOp::StartsWith,
                    "endswith" => TextOp::EndsWith,
                    other => {
                        return Err(PlanError(format!("unsupported function `{other}`")));
                    }
                };
                let [
                    Expr::Identifier(field_name),
                    Expr::Value(Literal::String(needle)),
                ] = args.as_slice()
                else {
                    return Err(PlanError(format!(
                        "`{name}` takes a field and a string literal"
                    )));
                };
                let field = self.field(field_name)?;
                if field.kind() != ScalarKind::String {
                    return Err(PlanError(format!(
                        "`{name}` applies to string fields; `{field_name}` is {}",
                        field.kind().as_str()
                    )));
                }
                Ok(Predicate::Text {
                    field,
                    op,
                    needle: needle.clone(),
                })
            }
            Expr::Identifier(name) => {
                Err(PlanError(format!("`{name}` is not a boolean expression")))
            }
            Expr::Value(_) => Err(PlanError("a bare literal is not a filter".to_owned())),
        }
    }
}

/// Check the literal against the field's kind and canonicalize it.
fn coerce(field: &FieldRef, literal: &Literal) -> Result<Scalar, PlanError> {
    let name = field.odata_name();
    let kind = field.kind();
    let mismatch = |got: &str| {
        PlanError(format!(
            "`{name}` is {}; a {got} literal does not compare with it",
            kind.as_str()
        ))
    };
    match (kind, literal) {
        (ScalarKind::String, Literal::String(s)) => Ok(Scalar::Str(s.clone())),
        (ScalarKind::Number | ScalarKind::Integer, Literal::Number(n)) => {
            Ok(Scalar::Num(n.normalized().to_string()))
        }
        (ScalarKind::Boolean, Literal::Bool(b)) => Ok(Scalar::Bool(*b)),
        (ScalarKind::DateTime, Literal::DateTime(dt)) => Ok(Scalar::DateTime(dt.to_rfc3339())),
        // A date-time may also be written as its RFC 3339 text.
        (ScalarKind::DateTime, Literal::String(s)) => Ok(Scalar::DateTime(s.clone())),
        (_, other) => Err(mismatch(&other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_odata::{ODataOrderBy, OrderKey};

    fn admitted() -> BTreeMap<String, ScalarKind> {
        BTreeMap::from([
            ("/payload/severity".to_owned(), ScalarKind::String),
            ("/payload/score".to_owned(), ScalarKind::Number),
        ])
    }

    fn query(filter: &str, order: &[(&str, SortDir)]) -> ODataQuery {
        let mut q = ODataQuery::new();
        if !filter.is_empty() {
            let parsed = toolkit_odata::parse_filter_string(filter).expect("filter parses");
            q = q.with_filter(parsed.into_expr());
        }
        q.with_order(ODataOrderBy(
            order
                .iter()
                .map(|(f, d)| OrderKey {
                    field: (*f).to_owned(),
                    dir: *d,
                })
                .collect(),
        ))
    }

    #[test]
    fn a_declared_path_resolves_with_its_kind_and_the_tiebreaker_is_appended() {
        let q = query(
            "payload/severity eq 'high' and payload/score gt 5",
            &[("payload/score", SortDir::Desc)],
        );
        let plan = plan(&q, Some(&admitted())).expect("plans");
        assert!(plan.references_payload());
        assert_eq!(
            plan.order,
            vec![
                OrderTerm {
                    field: FieldRef::Payload {
                        pointer: "/payload/score".into(),
                        kind: ScalarKind::Number
                    },
                    dir: SortDir::Desc
                },
                OrderTerm {
                    field: FieldRef::NodeKey,
                    dir: SortDir::Asc
                },
            ]
        );
        assert_eq!(
            plan.filter,
            Some(Predicate::And(vec![
                Predicate::Compare {
                    field: FieldRef::Payload {
                        pointer: "/payload/severity".into(),
                        kind: ScalarKind::String
                    },
                    op: CmpOp::Eq,
                    value: Scalar::Str("high".into()),
                },
                Predicate::Compare {
                    field: FieldRef::Payload {
                        pointer: "/payload/score".into(),
                        kind: ScalarKind::Number
                    },
                    op: CmpOp::Gt,
                    value: Scalar::Num("5".into()),
                },
            ]))
        );
    }

    #[test]
    fn an_undeclared_path_is_refused_naming_the_alternatives() {
        let q = query("payload/nope eq 'x'", &[]);
        let error = plan(&q, Some(&admitted())).expect_err("refused");
        assert!(error.0.contains("payload/nope"), "{error}");
        assert!(error.0.contains("payload/severity"), "{error}");
        assert!(error.0.contains("node_key"), "{error}");
    }

    #[test]
    fn a_payload_path_without_a_type_set_is_refused() {
        let q = query("payload/severity eq 'x'", &[]);
        let error = plan(&q, None).expect_err("refused");
        assert!(error.0.contains("type_pattern"), "{error}");
    }

    #[test]
    fn a_literal_of_the_wrong_kind_is_refused() {
        let q = query("payload/score eq 'five'", &[]);
        let error = plan(&q, Some(&admitted())).expect_err("refused");
        assert!(error.0.contains("number"), "{error}");
        assert!(error.0.contains("string literal"), "{error}");
    }

    #[test]
    fn column_only_queries_do_not_mention_payload() {
        let q = query("name eq 'a'", &[("created_at", SortDir::Desc)]);
        assert!(!mentions_payload(&q));
        let plan = plan(&q, None).expect("plans without a type set");
        assert!(!plan.references_payload());
        assert_eq!(plan.order.len(), 2);
    }

    #[test]
    fn a_path_counts_only_when_every_selected_type_declares_it_alike() {
        let a = BTreeMap::from([
            ("/payload/x".to_owned(), ScalarKind::String),
            ("/payload/y".to_owned(), ScalarKind::Number),
            ("/payload/z".to_owned(), ScalarKind::String),
        ]);
        let b = BTreeMap::from([
            ("/payload/x".to_owned(), ScalarKind::String),
            ("/payload/y".to_owned(), ScalarKind::String),
        ]);
        let common = admitted_paths(&[a, b]);
        assert_eq!(
            common,
            BTreeMap::from([("/payload/x".to_owned(), ScalarKind::String)])
        );
        assert!(admitted_paths(&[]).is_empty());
    }
}
