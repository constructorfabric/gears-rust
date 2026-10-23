//! A stand-in for a later engine generation, shared by the registry's unit tests and the replay
//! test. The including module imports the `rating-core` names it uses.

use time::OffsetDateTime;

use super::{
    ArithmeticBudget, CoreUsagePolicy, Engine, EngineId, EvaluationError, EvaluationInput,
    EvaluationOutcome, FactExtent, FloorPart, OrderEvaluation, OrderEvaluationInput, PriceTotal,
    RatingWindow, SplitInput,
};

/// Generation `.0`, with different semantics from generation 1: it rates every line as nothing, so
/// a replay that reached it would not reproduce generation 1's results.
pub struct Later(
    /// The generation number.
    pub u32,
);

impl Engine for Later {
    fn id(&self) -> EngineId {
        EngineId {
            engine_generation: self.0,
            engine_digest: format!("{:064x}", self.0),
        }
    }

    fn budget(&self) -> ArithmeticBudget {
        ArithmeticBudget::GEN_1
    }

    fn evaluate(&self, _: &EvaluationInput) -> Result<EvaluationOutcome, EvaluationError> {
        Ok(EvaluationOutcome {
            lines: Vec::new(),
            obligations: Vec::new(),
            price_totals: Vec::new(),
        })
    }

    fn evaluate_order(&self, _: &OrderEvaluationInput) -> Result<OrderEvaluation, EvaluationError> {
        Err(EvaluationError::UnsupportedPrimitive {
            detail: "a stand-in prices no order".into(),
        })
    }

    fn split_points(&self, _: &SplitInput) -> Vec<OffsetDateTime> {
        Vec::new()
    }

    fn window_geometry(
        &self,
        _: &CoreUsagePolicy,
        _: FactExtent,
    ) -> Result<Vec<RatingWindow>, EvaluationError> {
        Ok(Vec::new())
    }

    fn minimum_fee_floor(&self, _: &[PriceTotal]) -> Result<Vec<FloorPart>, EvaluationError> {
        Ok(Vec::new())
    }
}
