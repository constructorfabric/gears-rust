//! Evaluation of the applicable documents of one request.
//!
//! Synchronous and CPU-bound: the decision service runs it on a blocking task.
//! Tenant documents are all evaluated (no short-circuit) and built-in policies
//! up to the first denial, against one input and one timestamp, each under
//! what remains of the request's [`EvaluationBudget`]. Any error fails the
//! whole evaluation; a value that is neither a boolean nor undefined is an
//! error, never a permit.

use std::sync::Arc;

use admission_control_sdk::{EngineResultBuilder, PolicyReference};
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_policy_evaluation::{
    BoolEvaluationError, CompiledDocument, ContextError, EvaluationBudget, EvaluationContext,
    EvaluationError,
};
use uuid::Uuid;

use crate::domain::decision::compiled::CompiledDocumentEntry;

/// Why an evaluation failed.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvaluatorFailure {
    /// The request's evaluation budget ran out.
    #[error("evaluation exceeded its time budget at document `{document}`")]
    Timeout {
        /// Name of the document being evaluated when the budget ran out.
        document: String,
    },
    /// The backend failed evaluating a document.
    #[error("backend failed evaluating document `{document}`: {message}")]
    Backend {
        /// Name of the failing document.
        document: String,
        /// The backend's detail.
        message: String,
    },
    /// A document's `deny` is neither a boolean nor undefined.
    #[error("document `{document}` returned a value that is not a boolean")]
    Unmappable {
        /// Name of the offending document.
        document: String,
    },
    /// The evaluation context could not be built.
    #[error("the evaluation context could not be built: {0}")]
    Context(ContextError),
}

/// A document selected for a request, compiled, with its assignment's mode.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ApplicableDocument {
    /// The compiled document.
    pub compiled: Arc<dyn CompiledDocument>,
    /// Its identity, as reported to the admission gate.
    pub reference: PolicyReference,
    /// Whether its assignment enforces.
    pub enforce: bool,
}

/// What the Rego input is built from. The subject comes from the
/// `SecurityContext` only.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct RequestFacts {
    /// Requested action.
    pub action: String,
    /// GTS type id of the resource.
    pub resource_type: String,
    /// Resource id, where the request named one.
    pub resource_id: Option<Uuid>,
    /// Tenant owning the resource.
    pub resource_tenant_id: Uuid,
    /// Caller-supplied properties.
    pub properties: Map<String, Value>,
    /// Calling subject.
    pub subject_id: Uuid,
    /// The subject's tenant.
    pub subject_tenant_id: Uuid,
}

impl RequestFacts {
    /// The Rego `input` document.
    #[must_use]
    pub fn to_input(&self) -> Value {
        json!({
            "action": self.action,
            "resource": {
                "type": self.resource_type,
                "id": self.resource_id.map(|id| id.to_string()),
                "tenant_id": self.resource_tenant_id.to_string(),
            },
            "properties": Value::Object(self.properties.clone()),
            "subject": {
                "id": self.subject_id.to_string(),
                "tenant_id": self.subject_tenant_id.to_string(),
            },
        })
    }
}

/// Evaluates every document in `docs` under `budget` and records each denial:
/// an enforcing document's as a denial, a non-enforcing one's as a shadow
/// denial.
///
/// # Errors
///
/// [`EvaluatorFailure`] on the first document that fails or exceeds what
/// remains of the budget.
pub fn evaluate_documents(
    docs: &[ApplicableDocument],
    facts: &RequestFacts,
    evaluated_at: OffsetDateTime,
    budget: &mut EvaluationBudget,
) -> Result<EngineResultBuilder, EvaluatorFailure> {
    let ctx = context(facts, evaluated_at)?;
    let mut result = EngineResultBuilder::new();
    for doc in docs {
        if denies(
            budget,
            doc.compiled.as_ref(),
            &doc.reference.document_name,
            &ctx,
        )? {
            result.record_denial(doc.reference.clone(), doc.enforce);
        }
    }
    Ok(result)
}

/// Evaluates `docs` in order under `budget` and stops at the first denial,
/// returning its name; `None` when no document denies.
///
/// # Errors
///
/// [`EvaluatorFailure`] on the first document that fails or exceeds what
/// remains of the budget before a denial is found.
pub fn first_denial<'a>(
    docs: impl IntoIterator<Item = &'a CompiledDocumentEntry>,
    facts: &RequestFacts,
    evaluated_at: OffsetDateTime,
    budget: &mut EvaluationBudget,
) -> Result<Option<String>, EvaluatorFailure> {
    let ctx = context(facts, evaluated_at)?;
    for doc in docs {
        if denies(budget, doc.compiled.as_ref(), &doc.name, &ctx)? {
            return Ok(Some(doc.name.clone()));
        }
    }
    Ok(None)
}

fn context(
    facts: &RequestFacts,
    evaluated_at: OffsetDateTime,
) -> Result<EvaluationContext, EvaluatorFailure> {
    EvaluationContext::new(facts.to_input(), evaluated_at).map_err(EvaluatorFailure::Context)
}

/// Whether the document `name` denies, under what remains of `budget`.
fn denies(
    budget: &mut EvaluationBudget,
    compiled: &dyn CompiledDocument,
    name: &str,
    ctx: &EvaluationContext,
) -> Result<bool, EvaluatorFailure> {
    budget.evaluate_bool(compiled, ctx).map_err(|err| {
        let document = name.to_owned();
        match err {
            BoolEvaluationError::Evaluation(EvaluationError::BoundExceeded { .. }) => {
                EvaluatorFailure::Timeout { document }
            }
            BoolEvaluationError::Evaluation(EvaluationError::Failed { message }) => {
                EvaluatorFailure::Backend { document, message }
            }
            BoolEvaluationError::NotBoolean => EvaluatorFailure::Unmappable { document },
        }
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "evaluator_tests.rs"]
mod evaluator_tests;
