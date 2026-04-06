//! PEP constraint compiler.
//!
//! Compiles PDP evaluation responses into `AccessScope` for the secure ORM.
//!
//! ## Compilation Matrix (decision=true assumed)
//!
//! | `require_constraints` | constraints | Result |
//! |-------------------|-------------|--------|
//! | false             | empty       | `allow_all()` |
//! | false             | present     | Compile constraints → `AccessScope` |
//! | true              | empty       | `ConstraintsRequiredButAbsent` |
//! | true              | present     | Compile constraints → `AccessScope` |
//!
//! Unknown/unsupported properties fail that constraint (fail-closed).
//!
//! When `require_constraints=false`, empty constraints are treated as
//! `allow_all()` (legitimate PDP "yes, no row-level filtering"). When
//! `require_constraints=true`, empty constraints are an error (fail-closed).
//! If the PDP returns constraints regardless of the flag, they are compiled.

use modkit_security::{AccessScope, ScopeConstraint, ScopeFilter, ScopeValue};

use crate::constraints::{Constraint, Predicate};
use crate::models::EvaluationResponse;

/// Error during constraint compilation.
#[derive(Debug, thiserror::Error)]
pub enum ConstraintCompileError {
    /// Constraints were required but the PDP returned none.
    ///
    /// Per the design Decision Matrix, this is a deny: the PEP asked for
    /// row-level constraints but received an empty set. Fail-closed.
    #[error("constraints required but PDP returned none (fail-closed)")]
    ConstraintsRequiredButAbsent,

    /// All constraints contained unknown predicates (fail-closed).
    #[error("all constraints failed compilation (fail-closed): {reason}")]
    AllConstraintsFailed { reason: String },
}

/// Compile constraints from an evaluation response into an `AccessScope`.
///
/// **Precondition:** the caller has already verified `response.decision == true`.
/// This function only handles constraint compilation:
/// - `require_constraints=false, constraints=[]` → `Ok(allow_all())`
/// - `require_constraints=false, constraints=[..]` → compile predicates
/// - `require_constraints=true, constraints=[]` → `Err(ConstraintsRequiredButAbsent)`
/// - `require_constraints=true, constraints=[..]` → compile predicates
///
/// Each PDP constraint compiles to a `ScopeConstraint` (AND of filters).
/// Multiple constraints become `AccessScope::from_constraints` (OR-ed).
///
/// The compiler is property-agnostic: it validates predicates against the
/// provided `supported_properties` list and converts them structurally.
/// Unknown properties fail that constraint (fail-closed).
/// If ALL constraints fail compilation, returns `AllConstraintsFailed`.
///
/// # Errors
///
/// - `ConstraintsRequiredButAbsent` if constraints were required but empty
/// - `AllConstraintsFailed` if all constraints have unsupported predicates
pub fn compile_to_access_scope(
    response: &EvaluationResponse,
    require_constraints: bool,
    supported_properties: &[&str],
) -> Result<AccessScope, ConstraintCompileError> {
    // Step 1: Handle empty constraints based on require_constraints flag.
    if response.context.constraints.is_empty() {
        if require_constraints {
            return Err(ConstraintCompileError::ConstraintsRequiredButAbsent);
        }
        return Ok(AccessScope::allow_all());
    }

    // Step 2: Compile each constraint
    let mut constraints = Vec::new();
    let mut fail_reasons: Vec<String> = Vec::new();

    for constraint in &response.context.constraints {
        match compile_constraint(constraint, supported_properties) {
            Ok(sc) => constraints.push(sc),
            Err(reason) => {
                tracing::warn!(
                    reason = %reason,
                    "constraint compilation failed (fail-closed), possible PDP contract violation",
                );
                fail_reasons.push(reason);
            }
        }
    }

    // If no constraint compiled successfully, fail-closed
    if constraints.is_empty() {
        return Err(ConstraintCompileError::AllConstraintsFailed {
            reason: fail_reasons.join("; "),
        });
    }

    // If all compiled constraints are empty (no filters), it means allow-all
    if constraints.iter().all(ScopeConstraint::is_empty) {
        return Ok(AccessScope::allow_all());
    }

    Ok(AccessScope::from_constraints(constraints))
}

/// Compile a single PDP constraint into a `ScopeConstraint`.
///
/// Each predicate becomes a `ScopeFilter`. If any predicate's property
/// is not in `supported_properties`, the entire constraint fails (fail-closed).
fn compile_constraint(
    constraint: &Constraint,
    supported_properties: &[&str],
) -> Result<ScopeConstraint, String> {
    let mut filters = Vec::new();

    for predicate in &constraint.predicates {
        let (property, filter) = match predicate {
            Predicate::Eq(eq) => {
                let value = json_to_scope_value(&eq.value)?;
                (eq.property.as_str(), ScopeFilter::eq(&eq.property, value))
            }
            Predicate::In(p) => {
                let values: Vec<ScopeValue> = p
                    .values
                    .iter()
                    .map(json_to_scope_value)
                    .collect::<Result<_, _>>()?;
                (p.property.as_str(), ScopeFilter::r#in(&p.property, values))
            }
        };

        if !supported_properties.contains(&property) {
            return Err(format!("unsupported property: {property}"));
        }

        filters.push(filter);
    }

    Ok(ScopeConstraint::new(filters))
}

/// Convert a `serde_json::Value` to a `ScopeValue`.
///
/// UUID strings are detected and stored as `ScopeValue::Uuid`;
/// other strings become `ScopeValue::String`.
fn json_to_scope_value(v: &serde_json::Value) -> Result<ScopeValue, String> {
    match v {
        serde_json::Value::String(s) => {
            if let Ok(uuid) = uuid::Uuid::parse_str(s) {
                Ok(ScopeValue::Uuid(uuid))
            } else {
                Ok(ScopeValue::String(s.clone()))
            }
        }
        serde_json::Value::Number(n) => n.as_i64().map(ScopeValue::Int).ok_or_else(|| {
            format!("only integer JSON numbers are supported for scope filters, got: {n}")
        }),
        serde_json::Value::Bool(b) => Ok(ScopeValue::Bool(*b)),
        other => Err(format!(
            "unsupported JSON value type for scope filter: {other}"
        )),
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "compiler_tests.rs"]
mod tests;
