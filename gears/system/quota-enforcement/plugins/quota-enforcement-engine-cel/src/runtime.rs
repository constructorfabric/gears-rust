//! Metered evaluator for the explicitly accepted CEL AST profile.
//!
//! Every node charges the [`EvaluationMeter`] before it runs and every produced
//! value is measured against the value bounds, so the budget is enforced from
//! inside the evaluation rather than by a timeout that cannot preempt it.
//! Integer arithmetic is checked; overflow and division by zero are type
//! errors, never wraps.
use std::collections::BTreeMap;

use cel_core::types::{BinaryOp, ComprehensionData, Expr, SpannedExpr, UnaryOp};
use quota_enforcement_sdk::{EngineError, EvaluationMeter};
use serde_json::{Map, Value};

/// Largest value, measured as JSON bytes, an expression may build or be given.
pub const MAX_VALUE_BYTES: usize = 64 * 1024;
/// Most elements one list or map may hold.
pub const MAX_ITEMS: usize = 1024;
/// Deepest nesting one value may reach.
pub const MAX_DEPTH: usize = 48;

pub fn error(message: &str) -> EngineError {
    EngineError::TypeError(message.into())
}

/// Check size and depth before cloning external values or traversing recursive
/// JSON. The measure is also the cost charged for carrying the value.
pub fn measure(value: &Value) -> Result<u64, EngineError> {
    let mut pending = vec![(value, 0)];
    let mut bytes = 0_usize;
    while let Some((value, depth)) = pending.pop() {
        if depth > MAX_DEPTH {
            return Err(error("value nesting exceeds the CEL bound"));
        }
        bytes = bytes.checked_add(16).ok_or(EngineError::CostExceeded)?;
        match value {
            Value::String(s) => {
                bytes = bytes
                    .checked_add(s.len())
                    .ok_or(EngineError::CostExceeded)?;
            }
            Value::Array(a) => {
                if a.len() > MAX_ITEMS {
                    return Err(EngineError::CostExceeded);
                }
                pending.extend(a.iter().map(|v| (v, depth + 1)));
            }
            Value::Object(m) => {
                if m.len() > MAX_ITEMS {
                    return Err(EngineError::CostExceeded);
                }
                for (key, v) in m {
                    bytes = bytes
                        .checked_add(key.len())
                        .ok_or(EngineError::CostExceeded)?;
                    pending.push((v, depth + 1));
                }
            }
            _ => {}
        }
        if bytes > MAX_VALUE_BYTES {
            return Err(EngineError::CostExceeded);
        }
    }
    u64::try_from(bytes).map_err(|_| EngineError::CostExceeded)
}

fn integer(value: &Value) -> Result<i64, EngineError> {
    value
        .as_i64()
        .ok_or_else(|| error("expected signed 64-bit integer"))
}

fn boolean(value: &Value) -> Result<bool, EngineError> {
    value.as_bool().ok_or_else(|| error("expected boolean"))
}

fn membership(left: &Value, right: &Value) -> Result<Value, EngineError> {
    let found = match right {
        Value::Array(items) => items.contains(left),
        Value::Object(map) => map.contains_key(
            left.as_str()
                .ok_or_else(|| error("membership key must be a string"))?,
        ),
        _ => return Err(error("membership requires collection")),
    };
    Ok(Value::Bool(found))
}

/// `+` over lists, strings and integers, each bounded.
fn add(left: Value, right: Value) -> Result<Value, EngineError> {
    match (left, right) {
        (Value::Array(mut a), Value::Array(b)) => {
            if a.len().saturating_add(b.len()) > MAX_ITEMS {
                return Err(EngineError::CostExceeded);
            }
            a.extend(b);
            Ok(Value::Array(a))
        }
        (Value::String(mut a), Value::String(b)) => {
            if a.len().saturating_add(b.len()) > MAX_VALUE_BYTES {
                return Err(EngineError::CostExceeded);
            }
            a.push_str(&b);
            Ok(Value::String(a))
        }
        (a, b) => Ok(Value::from(
            integer(&a)?
                .checked_add(integer(&b)?)
                .ok_or_else(|| error("integer overflow"))?,
        )),
    }
}

fn compare(op: BinaryOp, left: &Value, right: &Value) -> Result<Value, EngineError> {
    let ordering = match (left, right) {
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => integer(left)?.cmp(&integer(right)?),
    };
    Ok(Value::Bool(match op {
        BinaryOp::Lt => ordering.is_lt(),
        BinaryOp::Le => !ordering.is_gt(),
        BinaryOp::Gt => ordering.is_gt(),
        _ => !ordering.is_lt(),
    }))
}

fn arithmetic(op: BinaryOp, left: &Value, right: &Value) -> Result<Value, EngineError> {
    let a = integer(left)?;
    let b = integer(right)?;
    let number = match op {
        BinaryOp::Sub => a.checked_sub(b),
        BinaryOp::Mul => a.checked_mul(b),
        BinaryOp::Div => a.checked_div(b),
        BinaryOp::Mod => a.checked_rem(b),
        _ => None,
    };
    number
        .map(Value::from)
        .ok_or_else(|| error("integer overflow or division by zero"))
}

/// One evaluation: the meter it charges and the variables in scope.
pub struct Runtime {
    pub meter: EvaluationMeter,
    pub variables: BTreeMap<String, Value>,
}

impl Runtime {
    /// Evaluate `expr`, charging one unit per node plus the measure of every
    /// value produced.
    pub fn eval(&mut self, expr: &SpannedExpr) -> Result<Value, EngineError> {
        self.meter.charge(1)?;
        let value = match &expr.node {
            Expr::Null => Value::Null,
            Expr::Bool(v) => Value::Bool(*v),
            Expr::Int(v) => Value::from(*v),
            Expr::String(v) => Value::String(v.clone()),
            Expr::Ident(name) | Expr::RootIdent(name) => self
                .variables
                .get(name)
                .cloned()
                .ok_or_else(|| error("unknown CEL variable"))?,
            Expr::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(&item.expr)?);
                }
                Value::Array(values)
            }
            Expr::Map(entries) => {
                let mut values = Map::new();
                for entry in entries {
                    let key = self
                        .eval(&entry.key)?
                        .as_str()
                        .ok_or_else(|| error("map keys must be strings"))?
                        .to_owned();
                    let value = self.eval(&entry.value)?;
                    if values.insert(key, value).is_some() {
                        return Err(error("duplicate map key"));
                    }
                }
                Value::Object(values)
            }
            Expr::Unary { op, expr } => {
                let value = self.eval(expr)?;
                match op {
                    UnaryOp::Not => Value::Bool(!boolean(&value)?),
                    UnaryOp::Neg => Value::from(
                        integer(&value)?
                            .checked_neg()
                            .ok_or_else(|| error("integer overflow"))?,
                    ),
                }
            }
            Expr::Binary { op, left, right } => {
                let left = self.eval(left)?;
                // Short-circuit before the right side is evaluated or charged.
                if *op == BinaryOp::And && !boolean(&left)? {
                    return Ok(Value::Bool(false));
                }
                if *op == BinaryOp::Or && boolean(&left)? {
                    return Ok(Value::Bool(true));
                }
                let right = self.eval(right)?;
                self.binary(*op, left, right)?
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                if boolean(&self.eval(cond)?)? {
                    self.eval(then_expr)?
                } else {
                    self.eval(else_expr)?
                }
            }
            Expr::Member { expr, field, .. } => self
                .eval(expr)?
                .as_object()
                .and_then(|m| m.get(field))
                .cloned()
                .ok_or_else(|| error("missing CEL field"))?,
            Expr::MemberTestOnly { expr, field } => Value::Bool(
                self.eval(expr)?
                    .as_object()
                    .is_some_and(|m| m.contains_key(field)),
            ),
            Expr::Index { expr, index, .. } => self.index(expr, index)?,
            Expr::Call { expr, args } => self.call_expr(expr, args)?,
            Expr::Bind {
                var_name,
                init,
                body,
            } => {
                let value = self.eval(init)?;
                let previous = self.variables.insert(var_name.clone(), value);
                let result = self.eval(body);
                self.restore(var_name, previous);
                result?
            }
            Expr::Comprehension(c) => self.comprehension(c)?,
            _ => return Err(error("unsupported compiled CEL node")),
        };
        self.meter.charge(measure(&value)?)?;
        Ok(value)
    }

    fn index(&mut self, expr: &SpannedExpr, index: &SpannedExpr) -> Result<Value, EngineError> {
        let value = self.eval(expr)?;
        let index = self.eval(index)?;
        let found = match value {
            Value::Array(items) => {
                let at = usize::try_from(integer(&index)?).map_err(|_| error("negative index"))?;
                items.get(at).cloned()
            }
            Value::Object(map) => map
                .get(
                    index
                        .as_str()
                        .ok_or_else(|| error("map index must be a string"))?,
                )
                .cloned(),
            _ => None,
        };
        found.ok_or_else(|| error("index outside collection"))
    }

    fn call_expr(
        &mut self,
        callee: &SpannedExpr,
        args: &[SpannedExpr],
    ) -> Result<Value, EngineError> {
        let (name, receiver) = match &callee.node {
            Expr::Ident(name) => (name.as_str(), None),
            Expr::Member { expr, field, .. } => (field.as_str(), Some(self.eval(expr)?)),
            _ => return Err(error("unsupported call")),
        };
        let mut values: Vec<Value> = receiver.into_iter().collect();
        for arg in args {
            values.push(self.eval(arg)?);
        }
        self.call(name, &values)
    }

    /// A macro expansion: iterate the range, charging one unit per element on
    /// top of the body's own nodes, so an unbounded range exhausts the budget
    /// instead of the clock.
    fn comprehension(&mut self, c: &ComprehensionData) -> Result<Value, EngineError> {
        let range = self.eval(&c.iter_range)?;
        let initial = self.eval(&c.accu_init)?;
        let old_acc = self.variables.insert(c.accu_var.clone(), initial);
        let old_iter = self.variables.get(&c.iter_var).cloned();
        let result = self.iterate(c, range);
        self.restore(&c.accu_var, old_acc);
        self.restore(&c.iter_var, old_iter);
        result
    }

    fn iterate(&mut self, c: &ComprehensionData, range: Value) -> Result<Value, EngineError> {
        let values = match range {
            Value::Array(v) => v,
            Value::Object(m) => m.into_iter().map(|(key, _)| Value::String(key)).collect(),
            _ => return Err(error("comprehension requires collection")),
        };
        for value in values {
            self.meter.charge(1)?;
            self.variables.insert(c.iter_var.clone(), value);
            if !boolean(&self.eval(&c.loop_condition)?)? {
                break;
            }
            let next = self.eval(&c.loop_step)?;
            self.variables.insert(c.accu_var.clone(), next);
        }
        self.eval(&c.result)
    }

    fn restore(&mut self, name: &str, value: Option<Value>) {
        if let Some(value) = value {
            self.variables.insert(name.to_owned(), value);
        } else {
            self.variables.remove(name);
        }
    }

    fn binary(&mut self, op: BinaryOp, left: Value, right: Value) -> Result<Value, EngineError> {
        self.meter.charge(
            measure(&left)?
                .checked_add(measure(&right)?)
                .ok_or(EngineError::CostExceeded)?,
        )?;
        match op {
            BinaryOp::Eq => Ok(Value::Bool(left == right)),
            BinaryOp::Ne => Ok(Value::Bool(left != right)),
            BinaryOp::And => Ok(Value::Bool(boolean(&left)? && boolean(&right)?)),
            BinaryOp::Or => Ok(Value::Bool(boolean(&left)? || boolean(&right)?)),
            BinaryOp::In => membership(&left, &right),
            BinaryOp::Add => add(left, right),
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => compare(op, &left, &right),
            BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                arithmetic(op, &left, &right)
            }
        }
    }

    fn call(&mut self, name: &str, values: &[Value]) -> Result<Value, EngineError> {
        for value in values {
            self.meter.charge(measure(value)?)?;
        }
        match (name, values) {
            ("size", [v]) => {
                let n = match v {
                    Value::Array(v) => v.len(),
                    Value::Object(v) => v.len(),
                    Value::String(v) => v.chars().count(),
                    _ => return Err(error("size requires collection or string")),
                };
                Ok(Value::from(
                    i64::try_from(n).map_err(|_| error("size overflow"))?,
                ))
            }
            ("@not_strictly_false", [v]) => Ok(Value::Bool(v != &Value::Bool(false))),
            ("contains", [Value::String(a), Value::String(b)]) => Ok(Value::Bool(a.contains(b))),
            ("startsWith", [Value::String(a), Value::String(b)]) => {
                Ok(Value::Bool(a.starts_with(b)))
            }
            ("endsWith", [Value::String(a), Value::String(b)]) => Ok(Value::Bool(a.ends_with(b))),
            ("int", [Value::String(v)]) => v
                .parse::<i64>()
                .map(Value::from)
                .map_err(|_| error("invalid integer")),
            ("int", [v]) => Ok(Value::from(integer(v)?)),
            ("string", [Value::Number(v)]) => Ok(Value::String(v.to_string())),
            ("string", [Value::String(v)]) => Ok(Value::String(v.clone())),
            _ => Err(error("unsupported function or argument types")),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "runtime_tests.rs"]
mod tests;
