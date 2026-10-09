//! Structural save-time checker over the persisted schema environments.
//!
//! Infers a [`Shape`] for every node of the expanded AST against the variables
//! the metric's environment declares, so a reference to an absent property, a
//! scalar compared with a collection, or two fields whose declared domains
//! cannot intersect is refused when the policy is saved, with a line and
//! column into the operator's source. The decision record is checked on every
//! path: each branch of a conditional on its own, so a valid branch cannot
//! vouch for an invalid one.
use std::collections::BTreeMap;

use cel_core::types::{
    BinaryOp, ComprehensionData, Expr, ListElement, MapEntry, SpannedExpr, UnaryOp,
};
use quota_enforcement_sdk::{EngineConfigError, EnvironmentInputs};
use serde_json::Value;

/// Most nodes one expanded expression may hold.
const MAX_NODES: usize = 1024;
/// Deepest nesting one expanded expression or schema may reach.
const MAX_DEPTH: usize = 48;

/// The static kind of a value.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// Declared with a type the profile cannot evaluate: a `number`, a
    /// `$ref`, a `not`, a nullable scalar other than an integer, or the member
    /// of a free-form object. The field is reachable so `has()` can see it,
    /// and every operation on it is refused at save time.
    Unknown,
    Null,
    Bool,
    Int,
    /// An integer or `null`: the shape of a Quota's `cap` and `remaining`, and
    /// of `type: ["integer", "null"]`. Integer operators accept it; evaluation
    /// refuses the `null`.
    OptionalInt,
    String,
    List(Box<Shape>),
    /// A record whose keys are all known statically.
    Object(BTreeMap<String, Shape>),
    /// A map with computed keys, or a schema object without declared
    /// properties: decodable only at evaluation.
    Map,
}

/// A kind plus, when the schema or a literal fixes it, the finite set of values
/// it can take.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub kind: Kind,
    pub domain: Option<Vec<Value>>,
}

impl Shape {
    pub fn new(kind: Kind) -> Self {
        Self { kind, domain: None }
    }

    /// A literal: its domain is the one value it can take.
    pub fn literal(kind: Kind, value: Value) -> Self {
        Self {
            kind,
            domain: Some(vec![value]),
        }
    }

    pub fn list(item: Self) -> Self {
        Self::new(Kind::List(Box::new(item)))
    }
}

/// A save-time error pinned to a byte offset in the operator's source, as a
/// one-based line and column.
pub fn at(source: &str, offset: usize, message: &str) -> EngineConfigError {
    let prefix = source.get(..offset).unwrap_or("");
    EngineConfigError {
        message: message.into(),
        line: Some(prefix.bytes().filter(|b| *b == b'\n').count() + 1),
        column: Some(prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1),
    }
}

pub fn diagnostic(source: &str, node: &SpannedExpr, message: &str) -> EngineConfigError {
    at(source, node.span.start, message)
}

pub fn config_error(message: &str) -> EngineConfigError {
    EngineConfigError {
        message: message.into(),
        line: None,
        column: None,
    }
}

/// The shape a JSON schema admits. `allOf` merges object properties,
/// `anyOf`/`oneOf` keep what every non-null alternative agrees on, and a
/// construct the profile cannot type becomes [`Kind::Unknown`] rather than
/// refusing the contract: one exotic field must not block every policy on the
/// metric, only the expressions that touch it.
pub fn schema(raw: &Value, depth: usize) -> Result<Shape, EngineConfigError> {
    if depth > MAX_DEPTH {
        return Err(config_error("schema nesting exceeds the CEL bound"));
    }
    if let Some(parts) = raw.get("allOf").and_then(Value::as_array) {
        // An object with no declared properties is still an object: every
        // access on it is "absent", which is the check the caller wants.
        let mut fields = BTreeMap::new();
        let mut object = false;
        for part in parts {
            if let Kind::Object(properties) = schema(part, depth + 1)?.kind {
                object = true;
                fields.extend(properties);
            }
        }
        if object {
            return Ok(Shape::new(Kind::Object(fields)));
        }
    }
    for union in ["anyOf", "oneOf"] {
        if let Some(parts) = raw.get(union).and_then(Value::as_array) {
            let mut shapes = parts
                .iter()
                .map(|s| schema(s, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            // Optional absence does not make new properties available. At
            // evaluation the expression must guard an absent resource itself.
            shapes.retain(|s| s.kind != Kind::Null);
            let Some(first) = shapes.first().cloned() else {
                return Ok(Shape::new(Kind::Null));
            };
            return shapes.into_iter().skip(1).try_fold(first, common);
        }
    }
    // `type: ["integer", "null"]` is the nullable spelling; only a nullable
    // integer has a kind of its own, other nullable scalars are refused on use.
    let (declared, nullable) = match raw.get("type") {
        Some(Value::String(name)) => (Some(name.as_str()), false),
        Some(Value::Array(names)) => {
            let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
            (
                names.iter().copied().find(|name| *name != "null"),
                names.contains(&"null"),
            )
        }
        _ => (None, false),
    };
    let kind = match declared {
        Some("null") => Kind::Null,
        Some("boolean") if !nullable => Kind::Bool,
        Some("integer") if nullable => Kind::OptionalInt,
        Some("integer") => Kind::Int,
        Some("string") if !nullable => Kind::String,
        Some("array") if !nullable => {
            let items = raw
                .get("items")
                .ok_or_else(|| config_error("array schema requires typed items"))?;
            Kind::List(Box::new(schema(items, depth + 1)?))
        }
        Some("object") | None if !nullable && raw.get("properties").is_some() => {
            let fields = raw
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| config_error("object schema requires properties"))?;
            Kind::Object(
                fields
                    .iter()
                    .map(|(name, s)| Ok((name.clone(), schema(s, depth + 1)?)))
                    .collect::<Result<_, EngineConfigError>>()?,
            )
        }
        Some("object") if !nullable => Kind::Map,
        _ => Kind::Unknown,
    };
    let domain = raw
        .get("enum")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| raw.get("const").map(|v| vec![v.clone()]));
    Ok(Shape { kind, domain })
}

/// The shape a value may take when it comes from either `a` or `b`. Two
/// records keep only the fields both declare, each merged in turn, so a field
/// one alternative lacks cannot be relied on; the domain is the union of both
/// or open when either is open. Callers have already checked compatibility.
fn merge(a: Shape, b: Shape) -> Shape {
    let kind = match (a.kind, b.kind) {
        (Kind::Object(left), Kind::Object(right)) => Kind::Object(
            left.into_iter()
                .filter_map(|(name, shape)| {
                    right
                        .get(&name)
                        .map(|other| (name, merge(shape, other.clone())))
                })
                .collect(),
        ),
        (Kind::Object(_) | Kind::Map, Kind::Object(_) | Kind::Map) => Kind::Map,
        (Kind::List(left), Kind::List(right)) => Kind::List(Box::new(merge(*left, *right))),
        (Kind::Int | Kind::OptionalInt, Kind::OptionalInt) | (Kind::OptionalInt, Kind::Int) => {
            Kind::OptionalInt
        }
        (Kind::Unknown, kind) | (kind, _) => kind,
    };
    Shape {
        kind,
        domain: union(a.domain, b.domain),
    }
}

fn union(a: Option<Vec<Value>>, b: Option<Vec<Value>>) -> Option<Vec<Value>> {
    match (a, b) {
        (Some(mut left), Some(right)) => {
            left.extend(right);
            Some(left)
        }
        _ => None,
    }
}

/// Whether two shapes may hold the same value. `Unknown` here is only the
/// element type of an empty list literal; an `Unknown` field is refused before
/// it reaches a comparison.
pub fn compatible(a: &Shape, b: &Shape) -> bool {
    match (&a.kind, &b.kind) {
        (Kind::List(a), Kind::List(b)) => compatible(a, b),
        // Records agree when every field both declare agrees.
        (Kind::Object(left), Kind::Object(right)) => left
            .iter()
            .all(|(name, shape)| right.get(name).is_none_or(|other| compatible(shape, other))),
        (Kind::Unknown, _)
        | (_, Kind::Unknown)
        | (Kind::Int | Kind::OptionalInt, Kind::Int | Kind::OptionalInt)
        | (Kind::Object(_) | Kind::Map, Kind::Object(_) | Kind::Map) => true,
        _ => a.kind == b.kind,
    }
}

/// What two schema alternatives agree on.
fn common(a: Shape, b: Shape) -> Result<Shape, EngineConfigError> {
    let kind = match (a.kind, b.kind) {
        (Kind::Object(a), Kind::Object(b)) => Kind::Object(
            a.into_iter()
                .filter_map(|(name, value)| {
                    b.get(&name)
                        .and_then(|other| common(value, other.clone()).ok())
                        .map(|s| (name, s))
                })
                .collect(),
        ),
        (Kind::List(a), Kind::List(b)) => Kind::List(Box::new(common(*a, *b)?)),
        (a, b) if a == b => a,
        _ => return Err(config_error("ambiguous types across schema alternatives")),
    };
    Ok(Shape {
        kind,
        domain: union(a.domain, b.domain),
    })
}

fn is_int_like(shape: &Shape) -> bool {
    matches!(shape.kind, Kind::Int | Kind::OptionalInt)
}

/// An `Unknown` operand is a field the profile cannot type; using it is an
/// error the operator can act on by declaring the field as a supported type.
fn refuse_unknown(
    source: &str,
    node: &SpannedExpr,
    shapes: &[&Shape],
) -> Result<(), EngineConfigError> {
    if shapes.iter().any(|shape| shape.kind == Kind::Unknown) {
        return Err(diagnostic(
            source,
            node,
            "operand is declared with a type outside the integer-only CEL profile",
        ));
    }
    Ok(())
}

/// Equality and membership between two fields: the kinds must be able to hold
/// the same value, and declared domains must intersect, or the comparison is
/// permanently false and the Quota it guards permanently inert (ADR-0007).
fn pair_check(
    source: &str,
    node: &SpannedExpr,
    a: &Shape,
    b: &Shape,
) -> Result<(), EngineConfigError> {
    refuse_unknown(source, node, &[a, b])?;
    if !compatible(a, b) && a.kind != Kind::Null && b.kind != Kind::Null {
        return Err(diagnostic(
            source,
            node,
            "paired fields have incompatible types or cardinality",
        ));
    }
    if let (Some(left), Some(right)) = (&a.domain, &b.domain)
        && !left.iter().any(|v| right.contains(v))
    {
        return Err(diagnostic(
            source,
            node,
            "paired fields have disjoint declared domains",
        ));
    }
    Ok(())
}

/// The element shape `a in b` compares `a` against.
fn member_of<'s>(
    source: &str,
    node: &SpannedExpr,
    a: &'s Shape,
    b: &'s Shape,
) -> Result<&'s Shape, EngineConfigError> {
    match &b.kind {
        Kind::List(item) => Ok(item),
        Kind::Object(_) | Kind::Map if a.kind == Kind::String => Ok(a),
        _ => Err(diagnostic(
            source,
            node,
            "membership requires a compatible collection",
        )),
    }
}

fn comparison(
    source: &str,
    node: &SpannedExpr,
    a: &Shape,
    b: &Shape,
) -> Result<Shape, EngineConfigError> {
    refuse_unknown(source, node, &[a, b])?;
    let comparable = |s: &Shape| matches!(s.kind, Kind::Int | Kind::OptionalInt | Kind::String);
    if comparable(a) && comparable(b) && compatible(a, b) {
        Ok(Shape::new(Kind::Bool))
    } else {
        Err(diagnostic(
            source,
            node,
            "comparison requires matching scalar types",
        ))
    }
}

fn arithmetic(
    source: &str,
    node: &SpannedExpr,
    a: &Shape,
    b: &Shape,
) -> Result<Shape, EngineConfigError> {
    refuse_unknown(source, node, &[a, b])?;
    if is_int_like(a) && is_int_like(b) {
        Ok(Shape::new(Kind::Int))
    } else {
        Err(diagnostic(
            source,
            node,
            "arithmetic requires integer operands",
        ))
    }
}

/// The result of one function in the bounded profile, or `None` when the
/// name or the argument kinds are outside it.
fn overload(name: &str, shapes: &[Shape]) -> Option<Shape> {
    let string = |s: &Shape| s.kind == Kind::String;
    let scalar = |s: &Shape| matches!(s.kind, Kind::Int | Kind::String);
    match (name, shapes) {
        ("size", [s])
            if matches!(
                s.kind,
                Kind::String | Kind::List(_) | Kind::Object(_) | Kind::Map
            ) =>
        {
            Some(Shape::new(Kind::Int))
        }
        ("@not_strictly_false", [s]) if s.kind == Kind::Bool => Some(Shape::new(Kind::Bool)),
        ("contains" | "startsWith" | "endsWith", [a, b]) if string(a) && string(b) => {
            Some(Shape::new(Kind::Bool))
        }
        ("int", [s]) if scalar(s) => Some(Shape::new(Kind::Int)),
        ("string", [s]) if scalar(s) => Some(Shape::new(Kind::String)),
        _ => None,
    }
}

/// The decision record contract on a statically known shape.
///
/// A computed-key record (`Map`) can only be decoded at evaluation and is
/// admitted; the runtime refuses it strictly there. A literal record must carry
/// exactly one of `debit_plan` and `deny`, each of the documented shape, with
/// every required field present on every element.
fn decision_record(
    source: &str,
    node: &SpannedExpr,
    shape: &Shape,
) -> Result<(), EngineConfigError> {
    let fail = |message: &str| diagnostic(source, node, message);
    let fields = match &shape.kind {
        Kind::Map => return Ok(()),
        Kind::Object(fields) => fields,
        _ => return Err(fail("expression must return a decision record")),
    };
    if fields
        .keys()
        .any(|key| key != "debit_plan" && key != "deny")
    {
        return Err(fail("decision record allows only `debit_plan` or `deny`"));
    }
    match (fields.get("debit_plan"), fields.get("deny")) {
        (Some(plan), None) => debit_plan_record(source, node, plan),
        (None, Some(deny)) => deny_record(source, node, deny),
        (Some(_), Some(_)) => Err(fail("decision record carries both `debit_plan` and `deny`")),
        (None, None) => Err(fail("decision record needs `debit_plan` or `deny`")),
    }
}

fn debit_plan_record(
    source: &str,
    node: &SpannedExpr,
    plan: &Shape,
) -> Result<(), EngineConfigError> {
    let fail = |message: &str| diagnostic(source, node, message);
    let Kind::List(item) = &plan.kind else {
        return Err(fail(
            "`debit_plan` must be a list of `{id, amount}` records",
        ));
    };
    match &item.kind {
        // An empty literal list has no element type; it decodes to a denial.
        Kind::Unknown | Kind::Map => Ok(()),
        Kind::Object(fields) => {
            let id = fields.get("id").is_some_and(|s| s.kind == Kind::String);
            // A nullable integer is an integer operand everywhere else in the
            // profile; here too, and evaluation refuses an actual `null`.
            let amount = fields
                .get("amount")
                .is_some_and(|s| matches!(s.kind, Kind::Int | Kind::OptionalInt));
            if id && amount && fields.len() == 2 {
                Ok(())
            } else {
                Err(fail(
                    "every `debit_plan` entry needs exactly `id: string` and `amount: int`",
                ))
            }
        }
        _ => Err(fail(
            "`debit_plan` must be a list of `{id, amount}` records",
        )),
    }
}

fn deny_record(source: &str, node: &SpannedExpr, deny: &Shape) -> Result<(), EngineConfigError> {
    let fail = |message: &str| diagnostic(source, node, message);
    match &deny.kind {
        Kind::Map => Ok(()),
        Kind::Object(fields) => {
            let reason = fields.get("reason").is_some_and(|s| s.kind == Kind::String);
            let ids = fields.get("violated_quota_ids").is_none_or(|s| {
                matches!(&s.kind, Kind::List(item) if matches!(item.kind, Kind::String | Kind::Unknown))
            });
            let known = fields
                .keys()
                .all(|k| k == "reason" || k == "violated_quota_ids");
            if reason && ids && known {
                Ok(())
            } else {
                Err(fail(
                    "`deny` needs `reason: string` and optionally `violated_quota_ids: list<string>`",
                ))
            }
        }
        _ => Err(fail(
            "`deny` must be a `{reason, violated_quota_ids}` record",
        )),
    }
}

/// One pass over an expression against one metric's environment.
pub struct Checker<'a> {
    pub source: &'a str,
    pub variables: BTreeMap<String, Shape>,
    pub nodes: usize,
    /// Which environment inputs the expression read.
    pub inputs: EnvironmentInputs,
}

impl Checker<'_> {
    /// Check that `node` produces a decision record on every path: each branch
    /// of a conditional and the body of a binding is checked on its own, so a
    /// valid branch cannot vouch for an invalid one.
    pub fn check_decision(
        &mut self,
        node: &SpannedExpr,
        depth: usize,
    ) -> Result<(), EngineConfigError> {
        match &node.node {
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                if self.infer(cond, depth + 1)?.kind != Kind::Bool {
                    return Err(self.fail(node, "condition must be boolean"));
                }
                self.check_decision(then_expr, depth + 1)?;
                self.check_decision(else_expr, depth + 1)
            }
            Expr::Bind {
                var_name,
                init,
                body,
            } => {
                let value = self.infer(init, depth + 1)?;
                let old = self.variables.insert(var_name.clone(), value);
                let result = self.check_decision(body, depth + 1);
                self.restore(var_name, old);
                result
            }
            _ => {
                let shape = self.infer(node, depth)?;
                decision_record(self.source, node, &shape)
            }
        }
    }

    /// Record the typed input a name reads, so the persisted snapshot keeps
    /// every contract a later rebuild needs. Dot and bracket access are the
    /// same dependency; only these three names select a contract.
    fn note_input(&mut self, name: &str) {
        match name {
            "request" => self.inputs.request = true,
            "resource" => self.inputs.resource = true,
            "arbitration" => self.inputs.arbitration = true,
            _ => {}
        }
    }

    /// The shape `node` evaluates to, or the first reason it cannot be trusted.
    pub fn infer(&mut self, node: &SpannedExpr, depth: usize) -> Result<Shape, EngineConfigError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES || depth > MAX_DEPTH {
            return Err(self.fail(node, "expression exceeds AST bounds"));
        }
        let depth = depth + 1;
        match &node.node {
            Expr::Null => Ok(Shape::new(Kind::Null)),
            Expr::Bool(v) => Ok(Shape::literal(Kind::Bool, Value::Bool(*v))),
            Expr::Int(v) => Ok(Shape::literal(Kind::Int, Value::from(*v))),
            Expr::String(v) => Ok(Shape::literal(Kind::String, Value::String(v.clone()))),
            Expr::Ident(name) | Expr::RootIdent(name) => {
                self.note_input(name);
                self.variables.get(name).cloned().ok_or_else(|| {
                    self.fail(
                        node,
                        "unknown variable; principal and attribution are unavailable",
                    )
                })
            }
            Expr::List(items) => self.list(node, items, depth),
            Expr::Map(entries) => self.map(node, entries, depth),
            Expr::Unary { op, expr } => self.unary(node, *op, expr, depth),
            Expr::Binary { op, left, right } => self.binary(node, *op, left, right, depth),
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => self.ternary(node, cond, then_expr, else_expr, depth),
            Expr::Member {
                expr,
                field,
                optional,
            } => self.member(node, expr, field, *optional, depth),
            Expr::MemberTestOnly { expr, field } => self.has(node, expr, field, depth),
            Expr::Index {
                expr,
                index,
                optional,
            } => self.index(node, expr, index, *optional, depth),
            Expr::Call { expr, args } => self.call(node, expr, args, depth),
            Expr::Bind {
                var_name,
                init,
                body,
            } => self.bind(var_name, init, body, depth),
            Expr::Comprehension(c) => self.comprehension(node, c, depth),
            _ => Err(self.fail(node, "node is outside the bounded integer CEL profile")),
        }
    }

    fn fail(&self, node: &SpannedExpr, message: &str) -> EngineConfigError {
        diagnostic(self.source, node, message)
    }

    fn list(
        &mut self,
        node: &SpannedExpr,
        items: &[ListElement],
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        let mut item_type: Option<Shape> = None;
        for item in items {
            if item.optional {
                return Err(self.fail(node, "optional list elements are unsupported"));
            }
            let shape = self.infer(&item.expr, depth)?;
            refuse_unknown(self.source, node, &[&shape])?;
            item_type = Some(match item_type {
                None => shape,
                Some(current) => {
                    if !compatible(&current, &shape) {
                        return Err(self.fail(node, "list elements have incompatible types"));
                    }
                    merge(current, shape)
                }
            });
        }
        Ok(Shape::list(
            item_type.unwrap_or_else(|| Shape::new(Kind::Unknown)),
        ))
    }

    fn map(
        &mut self,
        node: &SpannedExpr,
        entries: &[MapEntry],
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        let mut fields = BTreeMap::new();
        let mut computed = false;
        for entry in entries {
            if entry.optional {
                return Err(self.fail(node, "optional map entries are unsupported"));
            }
            if self.infer(&entry.key, depth)?.kind != Kind::String {
                return Err(self.fail(node, "map key must be a string"));
            }
            let shape = self.infer(&entry.value, depth)?;
            refuse_unknown(self.source, node, &[&shape])?;
            // Only a literal key names a field statically; one computed key
            // makes the whole record decodable only at evaluation.
            match &entry.key.node {
                Expr::String(key) => {
                    if fields.insert(key.clone(), shape).is_some() {
                        return Err(self.fail(node, "duplicate map key"));
                    }
                }
                _ => computed = true,
            }
        }
        Ok(Shape::new(if computed {
            Kind::Map
        } else {
            Kind::Object(fields)
        }))
    }

    fn unary(
        &mut self,
        node: &SpannedExpr,
        op: UnaryOp,
        expr: &SpannedExpr,
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        let shape = self.infer(expr, depth)?;
        refuse_unknown(self.source, node, &[&shape])?;
        let expected = if op == UnaryOp::Not {
            Kind::Bool
        } else {
            Kind::Int
        };
        // Negating a nullable Quota field is checked at evaluation.
        let accepted =
            shape.kind == expected || (expected == Kind::Int && shape.kind == Kind::OptionalInt);
        if !accepted {
            return Err(self.fail(node, "unary operator type mismatch"));
        }
        Ok(Shape::new(expected))
    }

    fn binary(
        &mut self,
        node: &SpannedExpr,
        op: BinaryOp,
        left: &SpannedExpr,
        right: &SpannedExpr,
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        let a = self.infer(left, depth)?;
        let b = self.infer(right, depth)?;
        let source = self.source;
        match op {
            // CEL's permissive heterogeneous equality would hide schema drift.
            BinaryOp::Eq | BinaryOp::Ne => {
                pair_check(source, node, &a, &b).map(|()| Shape::new(Kind::Bool))
            }
            BinaryOp::In => {
                let item = member_of(source, node, &a, &b)?;
                pair_check(source, node, &a, item).map(|()| Shape::new(Kind::Bool))
            }
            BinaryOp::And | BinaryOp::Or => {
                refuse_unknown(source, node, &[&a, &b])?;
                if a.kind == Kind::Bool && b.kind == Kind::Bool {
                    Ok(Shape::new(Kind::Bool))
                } else {
                    Err(self.fail(node, "logical operands must be boolean"))
                }
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                comparison(source, node, &a, &b)
            }
            BinaryOp::Add
                if compatible(&a, &b)
                    && a.kind != Kind::Unknown
                    && matches!(a.kind, Kind::List(_) | Kind::String) =>
            {
                Ok(merge(a, b))
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                arithmetic(source, node, &a, &b)
            }
        }
    }

    fn ternary(
        &mut self,
        node: &SpannedExpr,
        cond: &SpannedExpr,
        then_expr: &SpannedExpr,
        else_expr: &SpannedExpr,
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        if self.infer(cond, depth)?.kind != Kind::Bool {
            return Err(self.fail(node, "condition must be boolean"));
        }
        let a = self.infer(then_expr, depth)?;
        let b = self.infer(else_expr, depth)?;
        refuse_unknown(self.source, node, &[&a, &b])?;
        if !compatible(&a, &b) {
            return Err(self.fail(node, "conditional branches must have compatible types"));
        }
        Ok(merge(a, b))
    }

    fn member(
        &mut self,
        node: &SpannedExpr,
        expr: &SpannedExpr,
        field: &str,
        optional: bool,
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        if optional {
            return Err(self.fail(node, "optional select is unsupported; use has()"));
        }
        self.note_input(field);
        match self.infer(expr, depth)?.kind {
            Kind::Object(fields) => fields.get(field).cloned().ok_or_else(|| {
                self.fail(
                    node,
                    "property is absent or ambiguous in the persisted schemas",
                )
            }),
            // A free-form object's members are untyped until evaluation.
            Kind::Map => Ok(Shape::new(Kind::Unknown)),
            _ => Err(self.fail(node, "property access requires a typed object")),
        }
    }

    fn has(
        &mut self,
        node: &SpannedExpr,
        expr: &SpannedExpr,
        field: &str,
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        match self.infer(expr, depth)?.kind {
            Kind::Object(fields) if fields.contains_key(field) => Ok(Shape::new(Kind::Bool)),
            Kind::Map => Ok(Shape::new(Kind::Bool)),
            _ => Err(self.fail(node, "has() refers to an undeclared property")),
        }
    }

    fn index(
        &mut self,
        node: &SpannedExpr,
        expr: &SpannedExpr,
        index: &SpannedExpr,
        optional: bool,
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        if optional {
            return Err(self.fail(node, "optional indexing is unsupported"));
        }
        let shape = self.infer(expr, depth)?;
        let idx = self.infer(index, depth)?;
        if let Expr::String(key) = &index.node {
            // `q['arbitration']` reads the same contract as `q.arbitration`.
            self.note_input(key);
        }
        match shape.kind {
            Kind::List(item) if idx.kind == Kind::Int => Ok(*item),
            Kind::Object(fields) => match &index.node {
                Expr::String(key) => fields
                    .get(key)
                    .cloned()
                    .ok_or_else(|| self.fail(node, "unknown property")),
                _ => Err(self.fail(node, "dynamic object indexing would bypass schema checks")),
            },
            Kind::Map if idx.kind == Kind::String => Ok(Shape::new(Kind::Unknown)),
            _ => Err(self.fail(node, "index type mismatch")),
        }
    }

    fn call(
        &mut self,
        node: &SpannedExpr,
        callee: &SpannedExpr,
        args: &[SpannedExpr],
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        let (name, receiver) = match &callee.node {
            Expr::Ident(name) => (name.as_str(), None),
            Expr::Member { expr, field, .. } => (field.as_str(), Some(self.infer(expr, depth)?)),
            _ => return Err(self.fail(node, "unsupported function syntax")),
        };
        let mut shapes: Vec<Shape> = receiver.into_iter().collect();
        for arg in args {
            shapes.push(self.infer(arg, depth)?);
        }
        overload(name, &shapes).ok_or_else(|| {
            self.fail(
                node,
                "function or overload is outside the bounded CEL profile",
            )
        })
    }

    fn bind(
        &mut self,
        var_name: &str,
        init: &SpannedExpr,
        body: &SpannedExpr,
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        let value = self.infer(init, depth)?;
        let old = self.variables.insert(var_name.to_owned(), value);
        let result = self.infer(body, depth);
        self.restore(var_name, old);
        result
    }

    fn comprehension(
        &mut self,
        node: &SpannedExpr,
        c: &ComprehensionData,
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        if !c.iter_var2.is_empty() {
            return Err(self.fail(node, "two-variable comprehensions are unsupported"));
        }
        let item = match self.infer(&c.iter_range, depth)?.kind {
            Kind::List(item) => *item,
            Kind::Object(_) | Kind::Map => Shape::new(Kind::String),
            _ => return Err(self.fail(node, "comprehension requires collection")),
        };
        let initial = self.infer(&c.accu_init, depth)?;
        let old_item = self.variables.insert(c.iter_var.clone(), item);
        let old_acc = self.variables.insert(c.accu_var.clone(), initial);
        let result = self.comprehension_body(node, c, depth);
        self.restore(&c.iter_var, old_item);
        self.restore(&c.accu_var, old_acc);
        result
    }

    fn comprehension_body(
        &mut self,
        node: &SpannedExpr,
        c: &ComprehensionData,
        depth: usize,
    ) -> Result<Shape, EngineConfigError> {
        if self.infer(&c.loop_condition, depth)?.kind != Kind::Bool {
            return Err(self.fail(node, "comprehension condition must be boolean"));
        }
        let next = self.infer(&c.loop_step, depth)?;
        self.variables.insert(c.accu_var.clone(), next);
        self.infer(&c.result, depth)
    }

    fn restore(&mut self, name: &str, old: Option<Shape>) {
        if let Some(old) = old {
            self.variables.insert(name.into(), old);
        } else {
            self.variables.remove(name);
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "check_tests.rs"]
mod tests;
