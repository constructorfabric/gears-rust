//! PEP (Policy Enforcement Point) helpers.
//!
//! - [`PolicyEnforcer`] - PEP object (build → evaluate → compile)
//! - [`ResourceType`] - Static descriptor for a resource type + its supported properties
//! - [`compile_to_access_scope`] - Low-level: compile evaluation response into `AccessScope`
//! - [`IntoPropertyValue`] - Convert typed values into `serde_json::Value` for PDP requests

use serde_json::Value;
use uuid::Uuid;

pub mod compiler;
pub mod enforcer;

pub use compiler::{ConstraintCompileError, compile_to_access_scope};
pub use enforcer::{AccessRequest, EnforcerError, PolicyEnforcer, ResourceType};

/// Trait for types that can be converted into `serde_json::Value` for PDP
/// evaluation requests and predicate construction.
///
/// This trait lives at the PEP level - it converts typed values into JSON for
/// the authorization protocol. For scope-level typed values (post-compilation),
/// see [`modkit_security::ScopeValue`].
pub trait IntoPropertyValue {
    /// Convert into a `serde_json::Value` for use in authorization predicates.
    fn into_filter_value(self) -> Value;
}

impl IntoPropertyValue for Uuid {
    #[inline]
    fn into_filter_value(self) -> Value {
        Value::String(self.to_string())
    }
}

impl IntoPropertyValue for &Uuid {
    #[inline]
    fn into_filter_value(self) -> Value {
        Value::String(self.to_string())
    }
}

impl IntoPropertyValue for String {
    #[inline]
    fn into_filter_value(self) -> Value {
        Value::String(self)
    }
}

impl IntoPropertyValue for &str {
    #[inline]
    fn into_filter_value(self) -> Value {
        Value::String(self.to_owned())
    }
}

impl IntoPropertyValue for i64 {
    #[inline]
    fn into_filter_value(self) -> Value {
        Value::Number(self.into())
    }
}

impl IntoPropertyValue for bool {
    #[inline]
    fn into_filter_value(self) -> Value {
        Value::Bool(self)
    }
}

impl IntoPropertyValue for Value {
    #[inline]
    fn into_filter_value(self) -> Value {
        self
    }
}
