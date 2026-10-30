//! Built-in policies: platform rules from the gear's configuration.
//!
//! A built-in is a document like any tenant document (Rego defining the
//! boolean rule `deny`, selected by `resource_types` and `actions`), with three
//! differences: it applies to every request whatever the tenant, so no
//! assignment and no tenant barrier limits it; it runs before tenant policy,
//! and its denial ends the request; it is never stored, so it changes only
//! with the configuration. Built-ins run in configuration order and the first
//! denial wins.

use gts::GtsId;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_policy_evaluation::EvaluationBudget;

use super::compiled::{CompiledDocumentEntry, DocumentCompileError, compile_documents};
use crate::domain::eval::document_applies;
use crate::domain::evaluator::{EvaluatorFailure, RequestFacts, first_denial};
use crate::domain::model::Document;

/// The compiled built-in policies, in configuration order.
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct BuiltinPolicySet {
    policies: Vec<CompiledDocumentEntry>,
}

impl BuiltinPolicySet {
    /// Compiles `documents`, which [`ContentValidator`] has already accepted.
    ///
    /// # Errors
    ///
    /// [`DocumentCompileError`] for the first document that does not compile.
    ///
    /// [`ContentValidator`]: crate::domain::validation::ContentValidator
    pub fn compile(documents: &[Document]) -> Result<Self, DocumentCompileError> {
        Ok(Self {
            policies: compile_documents(documents)?,
        })
    }

    /// Number of policies.
    #[must_use]
    pub fn len(&self) -> usize {
        self.policies.len()
    }

    /// Whether the set holds no policy.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    fn applicable<'a>(
        &'a self,
        type_id: &'a GtsId,
        action: &'a str,
    ) -> impl Iterator<Item = &'a CompiledDocumentEntry> + 'a {
        self.policies
            .iter()
            .filter(move |p| document_applies(&p.resource_types, &p.actions, type_id, action))
    }

    /// Whether any policy applies to `action` on `type_id`.
    #[must_use]
    pub fn applies_to(&self, type_id: &GtsId, action: &str) -> bool {
        self.applicable(type_id, action).next().is_some()
    }

    /// Evaluates the applicable policies under `budget`, charging it: the
    /// name of the first that denies, or `None`.
    ///
    /// # Errors
    ///
    /// [`EvaluatorFailure`] when a policy fails or the budget runs out before
    /// a denial is found.
    pub fn first_denial(
        &self,
        type_id: &GtsId,
        facts: &RequestFacts,
        evaluated_at: OffsetDateTime,
        budget: &mut EvaluationBudget,
    ) -> Result<Option<String>, EvaluatorFailure> {
        first_denial(
            self.applicable(type_id, &facts.action),
            facts,
            evaluated_at,
            budget,
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "builtin_tests.rs"]
mod builtin_tests;
