//! Evaluation of the applicable documents of one request.
//!
//! Synchronous and CPU-bound: the decision service runs it on a blocking task.
//! Every document is evaluated (no short-circuit) against one input and one
//! timestamp, each under what remains of the request's wall-clock budget. Any
//! error fails the whole evaluation; a value that is neither a boolean nor
//! undefined is an error, never a permit.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_policy_evaluation::{CompiledDocument, CostBound, EvaluationContext, EvaluationError};
use uuid::Uuid;

use crate::domain::eval::{EvaluatedDocument, EvaluatedDocumentKey};

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
    #[error("backend failed evaluating document `{document}`")]
    Backend {
        /// Name of the failing document.
        document: String,
    },
    /// A document's `deny` is neither a boolean nor undefined.
    #[error("document `{document}` returned a value that is not a boolean")]
    Unmappable {
        /// Name of the offending document.
        document: String,
    },
    /// The evaluation context could not be built.
    #[error("the evaluation context could not be built")]
    Context,
}

/// A document selected for a request, compiled, with its assignment's mode.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ApplicableDocument {
    /// The compiled document.
    pub compiled: Arc<dyn CompiledDocument>,
    /// Its identity.
    pub key: EvaluatedDocumentKey,
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

/// Evaluates every document in `docs` within `budget`.
///
/// # Errors
///
/// [`EvaluatorFailure`] on the first document that fails or exceeds the
/// remaining budget.
pub fn evaluate_documents(
    docs: &[ApplicableDocument],
    facts: &RequestFacts,
    evaluated_at: OffsetDateTime,
    budget: Duration,
) -> Result<Vec<EvaluatedDocument>, EvaluatorFailure> {
    let ctx = EvaluationContext::new(facts.to_input(), evaluated_at)
        .map_err(|_| EvaluatorFailure::Context)?;
    let started = Instant::now();
    let mut out = Vec::with_capacity(docs.len());
    for doc in docs {
        let name = &doc.key.document_name;
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(EvaluatorFailure::Timeout {
                document: name.clone(),
            });
        }
        let denied = match doc.compiled.evaluate(&ctx, CostBound::new(remaining)) {
            Ok(Value::Bool(denied)) => denied,
            Ok(Value::Null) => false,
            Ok(_) => {
                return Err(EvaluatorFailure::Unmappable {
                    document: name.clone(),
                });
            }
            Err(EvaluationError::BoundExceeded { .. }) => {
                return Err(EvaluatorFailure::Timeout {
                    document: name.clone(),
                });
            }
            Err(EvaluationError::Failed { .. }) => {
                return Err(EvaluatorFailure::Backend {
                    document: name.clone(),
                });
            }
        };
        out.push(EvaluatedDocument {
            key: doc.key.clone(),
            enforce: doc.enforce,
            denied,
        });
    }
    Ok(out)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "evaluator_tests.rs"]
mod evaluator_tests;
