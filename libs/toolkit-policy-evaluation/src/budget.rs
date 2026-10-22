//! An evaluation-time budget shared by the evaluations of one request.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::backend::{CompiledDocument, CostBound, EvaluationContext, EvaluationError};

/// Evaluation time left for a sequence of evaluations.
///
/// Each evaluation runs under what the ones before it left, and only the time
/// spent inside [`Self::evaluate`] is charged: work between evaluations (I/O,
/// task hand-offs) does not consume the budget. Copyable, so a caller can move
/// it onto a blocking task and take the rest back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvaluationBudget {
    limit: Duration,
    remaining: Duration,
}

impl EvaluationBudget {
    /// A budget of `limit` with nothing spent.
    #[must_use]
    pub const fn new(limit: Duration) -> Self {
        Self {
            limit,
            remaining: limit,
        }
    }

    /// The budget's total.
    #[must_use]
    pub const fn limit(&self) -> Duration {
        self.limit
    }

    /// What is left of it.
    #[must_use]
    pub const fn remaining(&self) -> Duration {
        self.remaining
    }

    /// Whether nothing is left.
    #[must_use]
    pub const fn is_exhausted(&self) -> bool {
        self.remaining.is_zero()
    }

    /// Evaluates `doc` against `ctx` under what remains and charges the time
    /// the evaluation took.
    ///
    /// # Errors
    ///
    /// - [`EvaluationError::BoundExceeded`] carrying the budget's
    ///   [`limit`](Self::limit) when nothing remains (the document is not
    ///   evaluated) or the evaluation runs past what remains;
    /// - [`EvaluationError::Failed`] as [`CompiledDocument::evaluate`].
    ///
    /// `Ok(None)` is an undefined rule, as [`CompiledDocument::evaluate`].
    pub fn evaluate(
        &mut self,
        doc: &dyn CompiledDocument,
        ctx: &EvaluationContext,
    ) -> Result<Option<Value>, EvaluationError> {
        let exceeded = EvaluationError::BoundExceeded { limit: self.limit };
        if self.is_exhausted() {
            return Err(exceeded);
        }
        let started = Instant::now();
        let result = doc.evaluate(ctx, CostBound::new(self.remaining));
        self.remaining = self.remaining.saturating_sub(started.elapsed());
        result.map_err(|err| match err {
            EvaluationError::BoundExceeded { .. } => exceeded,
            failed @ EvaluationError::Failed { .. } => failed,
        })
    }

    /// Evaluates `doc` as a boolean rule, like [`Self::evaluate`]: `true` and
    /// `false` as they are, an undefined rule as `false`.
    ///
    /// # Errors
    ///
    /// - [`BoolEvaluationError::Evaluation`] as [`Self::evaluate`];
    /// - [`BoolEvaluationError::NotBoolean`] for any other value, including
    ///   an explicit `null`, which is never read as `false`.
    pub fn evaluate_bool(
        &mut self,
        doc: &dyn CompiledDocument,
        ctx: &EvaluationContext,
    ) -> Result<bool, BoolEvaluationError> {
        match self.evaluate(doc, ctx)? {
            Some(Value::Bool(value)) => Ok(value),
            None => Ok(false),
            Some(_) => Err(BoolEvaluationError::NotBoolean),
        }
    }
}

/// Why a boolean rule produced no value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoolEvaluationError {
    /// The evaluation itself failed or ran out of budget.
    #[error(transparent)]
    Evaluation(#[from] EvaluationError),
    /// The rule's value is neither a boolean nor undefined.
    #[error("the rule's value is neither a boolean nor undefined")]
    NotBoolean,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "budget_tests.rs"]
mod budget_tests;
