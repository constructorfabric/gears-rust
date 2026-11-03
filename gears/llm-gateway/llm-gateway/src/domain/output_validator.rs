//! Output Validator (DESIGN §3.2, `cpt-cf-llm-gateway-seq-structured-output-v1`):
//! validates a sync response's assembled output text against the ORIGINAL
//! request JSON schema.
//!
//! The assembled text is the concatenation, in order, of every `output_text`
//! part of every `message` output item. It must parse as JSON and validate
//! against the schema (workspace `jsonschema` crate). Failures are
//! `output_validation_error` whose message lists instance paths and failed
//! keywords only, never content values.

use llm_gateway_sdk::models::items::OutputItem;
use llm_gateway_sdk::{LlmGatewayError, OutputContentPart, ResponseResource};
use toolkit_macros::domain_model;

/// Most failures listed in one message.
const MAX_LISTED_FAILURES: usize = 16;

/// Concatenation of every `output_text` part of every `message` item.
fn assembled_text(resource: &ResponseResource) -> Option<String> {
    let mut text: Option<String> = None;
    for item in &resource.output {
        let OutputItem::Message(message) = item else {
            continue;
        };
        for part in &message.content {
            if let OutputContentPart::OutputText(part) = part {
                text.get_or_insert_with(String::new).push_str(&part.text);
            }
        }
    }
    text
}

/// Validates structured output.
#[domain_model]
#[derive(Debug, Clone, Copy, Default)]
pub struct OutputValidator;

impl OutputValidator {
    /// Validate the output text of `resource` against `schema`.
    ///
    /// # Errors
    ///
    /// `output_validation_error` for non-JSON output or a schema mismatch.
    #[allow(clippy::unused_self)] // instance method: the validator may gain configuration
    pub fn validate(
        self,
        schema: &serde_json::Value,
        resource: &ResponseResource,
    ) -> Result<(), LlmGatewayError> {
        let text = assembled_text(resource).ok_or_else(|| {
            LlmGatewayError::output_validation("the response has no output text to validate")
        })?;
        let instance: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| LlmGatewayError::output_validation("the output text is not valid JSON"))?;
        let validator = jsonschema::validator_for(schema).map_err(|_| {
            LlmGatewayError::output_validation("the response schema could not be compiled")
        })?;

        // Instance paths and failed keywords only: error text from the
        // validator may embed instance values.
        let mut failures: Vec<String> = Vec::new();
        let mut more = "";
        for error in validator
            .iter_errors(&instance)
            .take(MAX_LISTED_FAILURES + 1)
        {
            if failures.len() == MAX_LISTED_FAILURES {
                more = "; and more";
                break;
            }
            let path = error.instance_path().as_str();
            let path = if path.is_empty() { "/" } else { path };
            failures.push(format!("{path}: {}", error.kind().keyword()));
        }
        if failures.is_empty() {
            return Ok(());
        }
        let message = format!(
            "the output does not match the schema ({}{more})",
            failures.join("; ")
        );
        Err(LlmGatewayError::output_validation(message))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "output_validator_tests.rs"]
mod output_validator_tests;
