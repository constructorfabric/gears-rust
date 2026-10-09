//! Shared policy evaluation facility: a sandboxed Rego backend with syntax
//! validation separate from evaluation, a wall-clock evaluation bound,
//! parsed-form builtin introspection (for the denylist screen) and capability
//! isolation (an evaluation sees only its input and a caller-supplied
//! timestamp).
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

mod backend;
mod catalog;
mod rego;

pub use backend::{
    CompileError, CompiledDocument, ContextError, CostBound, EVALUATED_AT_KEY, EVALUATED_AT_NS_KEY,
    EvaluationBackend, EvaluationContext, EvaluationError,
};
pub use catalog::BackendCatalog;
pub use rego::{
    MAX_AST_DEPTH, MAX_BRACKET_NESTING, MAX_EVALUATION_DEPTH, MAX_RULE_DEPENDENCY_DEPTH,
    REGO_BACKEND_ID, REGO_DENYLIST, REGO_RESOURCE_DENYLIST, RegoBackend, screen_denylist,
};
