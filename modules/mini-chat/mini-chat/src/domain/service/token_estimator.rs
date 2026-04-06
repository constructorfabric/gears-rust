// Called from QuotaService which is not yet wired into the turn handler.
// Remove `dead_code` allows once QuotaService is live.

use modkit_macros::domain_model;

use crate::config::EstimationBudgets;

/// Input to the token estimation function.
#[domain_model]
#[allow(dead_code, clippy::struct_excessive_bools)]
pub struct EstimationInput {
    pub utf8_bytes: u64,
    pub num_images: u32,
    pub tools_enabled: bool,
    pub web_search_enabled: bool,
    pub code_interpreter_enabled: bool,
}

/// Result of token estimation.
#[domain_model]
#[allow(dead_code)]
pub struct EstimationResult {
    pub estimated_input_tokens: u64,
}

/// Estimate input tokens and reserve from request metadata.
///
/// Pure function — no I/O. Uses the estimation budgets from `ConfigMap`.
#[allow(dead_code)]
pub fn estimate_tokens(input: &EstimationInput, budgets: &EstimationBudgets) -> EstimationResult {
    // Step 1: text tokens from byte count
    let bpt = u64::from(budgets.bytes_per_token_conservative);
    let base_text_tokens = if input.utf8_bytes == 0 {
        u64::from(budgets.fixed_overhead_tokens)
    } else {
        input
            .utf8_bytes
            .div_ceil(bpt)
            .saturating_add(u64::from(budgets.fixed_overhead_tokens))
    };

    // Step 2: apply safety margin using integer math (multiply first, then div_ceil)
    let estimated_text_tokens = base_text_tokens
        .saturating_mul(100 + u64::from(budgets.safety_margin_pct))
        .div_ceil(100);

    // Step 3: surcharges
    let image_surcharge =
        u64::from(input.num_images).saturating_mul(u64::from(budgets.image_token_budget));
    let tool_surcharge = if input.tools_enabled {
        u64::from(budgets.tool_surcharge_tokens)
    } else {
        0
    };
    let web_search_surcharge = if input.web_search_enabled {
        u64::from(budgets.web_search_surcharge_tokens)
    } else {
        0
    };
    let code_interpreter_surcharge = if input.code_interpreter_enabled {
        u64::from(budgets.code_interpreter_surcharge_tokens)
    } else {
        0
    };

    // Step 4: totals
    let estimated_input_tokens = estimated_text_tokens
        .saturating_add(image_surcharge)
        .saturating_add(tool_surcharge)
        .saturating_add(web_search_surcharge)
        .saturating_add(code_interpreter_surcharge);

    EstimationResult {
        estimated_input_tokens,
    }
}
#[cfg(test)]
#[path = "token_estimator_tests.rs"]
mod tests;
