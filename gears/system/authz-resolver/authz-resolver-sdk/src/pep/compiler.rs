// Updated: 2026-04-14 by Constructor Tech
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
//!
//! ## Empty value lists (fail-closed)
//!
//! Set-membership predicates (`In`, `InGroup`, `InGroupSubtree`) with an
//! empty value list are rejected at compile time. An empty list means
//! "match nothing" which is semantically a deny — but passing it through
//! to the ORM would generate `WHERE col IN ()`, which is a SQL error on
//! some engines. Instead the compiler treats this as a PDP contract
//! violation and fails the constraint (fail-closed).
//!
//! `InTenantSubtree` carries a single `root_tenant_id` rather than a list,
//! so the empty-list case does not arise; a missing or non-UUID root id
//! is rejected the same way. Its optional `descendant_status` list is
//! converted element-wise (via `TenantStatus::as_smallint`) and bound to
//! the SQL `descendant_status` column; an empty list disables the filter.

use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, ScopeValue};

use crate::constraints::{Constraint, Predicate};
use crate::models::{BarrierMode, EvaluationResponse};

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
/// Unknown properties fail that constraint (fail-closed). Native group
/// predicates also fail closed through this entry point because their RG
/// member-handle type is absent; use
/// [`compile_to_access_scope_with_group_membership_type`] for those predicates.
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
    compile_to_access_scope_with_group_membership_type(
        response,
        require_constraints,
        supported_properties,
        None,
    )
}

/// Compile constraints with the RG member-handle type required by native group
/// predicates.
///
/// Kept separate from [`compile_to_access_scope`] so existing low-level callers
/// remain source-compatible. A native `InGroup`/`InGroupSubtree` predicate sent
/// through the untyped entry point fails closed instead of querying membership
/// rows across unrelated resource types.
///
/// # Errors
///
/// Returns the same errors as [`compile_to_access_scope`]. Native group
/// predicates additionally fail compilation when `group_membership_type` is
/// absent or empty.
pub fn compile_to_access_scope_with_group_membership_type(
    response: &EvaluationResponse,
    require_constraints: bool,
    supported_properties: &[&str],
    group_membership_type: Option<&str>,
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
        match compile_constraint(constraint, supported_properties, group_membership_type) {
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
    group_membership_type: Option<&str>,
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
                if values.is_empty() {
                    return Err(format!(
                        "In predicate on '{}' has empty value list (fail-closed)",
                        p.property
                    ));
                }
                (p.property.as_str(), ScopeFilter::r#in(&p.property, values))
            }
            Predicate::InGroup(p) => {
                let group_ids: Vec<ScopeValue> = p
                    .group_ids
                    .iter()
                    .map(json_to_scope_value)
                    .collect::<Result<_, _>>()?;
                if group_ids.is_empty() {
                    return Err(format!(
                        "InGroup predicate on '{}' has empty group_ids (fail-closed)",
                        p.property
                    ));
                }
                let membership_type =
                    required_group_membership_type(group_membership_type, "InGroup", &p.property)?;
                (
                    p.property.as_str(),
                    ScopeFilter::in_group_typed(&p.property, membership_type, group_ids),
                )
            }
            Predicate::InGroupSubtree(p) => {
                let ancestor_ids: Vec<ScopeValue> = p
                    .ancestor_ids
                    .iter()
                    .map(json_to_scope_value)
                    .collect::<Result<_, _>>()?;
                if ancestor_ids.is_empty() {
                    return Err(format!(
                        "InGroupSubtree predicate on '{}' has empty ancestor_ids (fail-closed)",
                        p.property
                    ));
                }
                let membership_type = required_group_membership_type(
                    group_membership_type,
                    "InGroupSubtree",
                    &p.property,
                )?;
                (
                    p.property.as_str(),
                    ScopeFilter::in_group_subtree_typed(&p.property, membership_type, ancestor_ids),
                )
            }
            Predicate::InTenantSubtree(p) => {
                let root_tenant_id = json_to_uuid_scope_value(&p.root_tenant_id).map_err(|e| {
                    format!(
                        "InTenantSubtree predicate on '{}' has invalid root_tenant_id: {e}",
                        p.property
                    )
                })?;
                // Map authz-sdk barrier mode onto toolkit-security's bool flag.
                // `Respect` (default) clamps the closure subquery with
                // `AND barrier = 0`; `Ignore` is reserved for cross-barrier
                // operations such as billing or tenant metadata reads.
                let respect_barriers = matches!(p.barrier_mode, BarrierMode::Respect);
                // Each `TenantStatus` lowers to its canonical SMALLINT
                // encoding (Active=1, Suspended=2, Deleted=3) so the SQL
                // bind matches the `tenant_closure.descendant_status`
                // column domain. The `Provisioning` AM-internal status is
                // not part of `TenantStatus` and therefore cannot be
                // expressed here — that matches the closure invariant.
                let descendant_status: Vec<ScopeValue> = p
                    .descendant_status
                    .iter()
                    .map(|s| ScopeValue::Int(i64::from(s.as_smallint())))
                    .collect();
                (
                    p.property.as_str(),
                    ScopeFilter::in_tenant_subtree(
                        &p.property,
                        root_tenant_id,
                        respect_barriers,
                        descendant_status,
                    ),
                )
            }
        };

        if !supported_properties.contains(&property) {
            return Err(format!("unsupported property: {property}"));
        }

        filters.push(filter);
    }

    Ok(ScopeConstraint::new(filters))
}

/// Return the configured RG member-handle type or a fail-closed compilation
/// reason suitable for the enclosing constraint.
fn required_group_membership_type<'a>(
    group_membership_type: Option<&'a str>,
    predicate: &str,
    property: &str,
) -> Result<&'a str, String> {
    group_membership_type.filter(|value| !value.is_empty()).ok_or_else(|| {
        format!(
            "{predicate} predicate on '{property}' requires a configured RG membership resource type (fail-closed)"
        )
    })
}

/// Convert a `serde_json::Value` to a UUID `ScopeValue`.
///
/// Only valid UUID strings are accepted; anything else (non-UUID string,
/// number, bool, null, array, object) is rejected. Used for `root_tenant_id`
/// in `InTenantSubtree` where the column type is always UUID.
fn json_to_uuid_scope_value(v: &serde_json::Value) -> Result<ScopeValue, String> {
    match v {
        serde_json::Value::String(s) => uuid::Uuid::parse_str(s)
            .map(ScopeValue::Uuid)
            .map_err(|_| format!("root_tenant_id must be a UUID string, got: {s:?} (fail-closed)")),
        serde_json::Value::Number(_) => {
            Err("root_tenant_id must be a UUID string, got number (fail-closed)".to_owned())
        }
        serde_json::Value::Bool(_) => {
            Err("root_tenant_id must be a UUID string, got bool (fail-closed)".to_owned())
        }
        other => Err(format!(
            "root_tenant_id must be a UUID string, got: {other} (fail-closed)"
        )),
    }
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
#[path = "compiler_tests.rs"]
mod compiler_tests;
