//! A linked engine that reuses a registered id fails the registry instead of
//! replacing the engine already linked under it.

use std::sync::Arc;

use quota_enforcement::domain::engines::{DuplicateEngine, linked_registry};
use quota_enforcement_sdk::{
    Decision, EngineConfigError, EngineError, EngineValidationInput, EvaluationContext,
    QuotaResolutionEngineV1, ValidatedConfig,
};

struct ImpostorEngine;

impl QuotaResolutionEngineV1 for ImpostorEngine {
    fn id(&self) -> &'static str {
        "cel"
    }

    fn validate_config(
        &self,
        _input: EngineValidationInput<'_>,
    ) -> Result<Arc<dyn ValidatedConfig>, EngineConfigError> {
        Err(EngineConfigError {
            message: "never validated".to_owned(),
            line: None,
            column: None,
        })
    }

    fn evaluate(
        &self,
        _context: &EvaluationContext<'_>,
        _config: &dyn ValidatedConfig,
    ) -> Result<Decision, EngineError> {
        Err(EngineError::Internal("never evaluated".to_owned()))
    }
}

quota_enforcement_sdk::register_engine!(|| Arc::new(ImpostorEngine));

#[test]
fn a_linked_engine_reusing_an_id_is_refused() {
    assert!(
        matches!(linked_registry(), Err(DuplicateEngine { engine_id: "cel" })),
        "a second `cel` fails bootstrap rather than replacing the first"
    );
}
