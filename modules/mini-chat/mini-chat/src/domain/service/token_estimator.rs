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
mod tests {
    use super::*;

    fn default_budgets() -> EstimationBudgets {
        EstimationBudgets {
            bytes_per_token_conservative: 4,
            fixed_overhead_tokens: 100,
            safety_margin_pct: 10,
            image_token_budget: 1000,
            tool_surcharge_tokens: 500,
            web_search_surcharge_tokens: 500,
            code_interpreter_surcharge_tokens: 1000,
            minimal_generation_floor: 50,
        }
    }

    #[test]
    fn text_only_estimation() {
        let input = EstimationInput {
            utf8_bytes: 4000,
            num_images: 0,
            tools_enabled: false,
            web_search_enabled: false,
            code_interpreter_enabled: false,
        };
        let result = estimate_tokens(&input, &default_budgets());

        // base = ceil(4000/4) + 100 = 1000 + 100 = 1100
        // with margin = ceil(1100 * 1.10) = ceil(1210.0) = 1210
        assert_eq!(result.estimated_input_tokens, 1210);
    }

    #[test]
    fn image_surcharge_stacking() {
        let input = EstimationInput {
            utf8_bytes: 0,
            num_images: 3,
            tools_enabled: false,
            web_search_enabled: false,
            code_interpreter_enabled: false,
        };
        let result = estimate_tokens(&input, &default_budgets());

        // base = 0 + 100 = 100, with margin = ceil(100 * 110 / 100) = 110
        // images = 3 * 1000 = 3000
        assert_eq!(result.estimated_input_tokens, 110 + 3000);
    }

    #[test]
    fn tool_and_web_search_surcharges() {
        let input = EstimationInput {
            utf8_bytes: 0,
            num_images: 0,
            tools_enabled: true,
            web_search_enabled: true,
            code_interpreter_enabled: false,
        };
        let result = estimate_tokens(&input, &default_budgets());

        // base = 100, with margin = 110, + tool 500 + web 500
        assert_eq!(result.estimated_input_tokens, 110 + 500 + 500);
    }

    #[test]
    fn all_surcharges_combined() {
        let input = EstimationInput {
            utf8_bytes: 4000,
            num_images: 2,
            tools_enabled: true,
            web_search_enabled: true,
            code_interpreter_enabled: false,
        };
        let result = estimate_tokens(&input, &default_budgets());

        // text: ceil(4000/4)+100 = 1100, margin: ceil(1100.0*1.1)=1210
        // images: 2*1000=2000, tool: 500, web: 500
        assert_eq!(result.estimated_input_tokens, 1210 + 2000 + 500 + 500);
    }

    #[test]
    fn zero_bytes_edge_case() {
        let input = EstimationInput {
            utf8_bytes: 0,
            num_images: 0,
            tools_enabled: false,
            web_search_enabled: false,
            code_interpreter_enabled: false,
        };
        let result = estimate_tokens(&input, &default_budgets());

        // base = 100 (overhead only), margin: ceil(100*110/100) = 110
        assert_eq!(result.estimated_input_tokens, 110);
    }

    #[test]
    fn code_interpreter_surcharge() {
        let input = EstimationInput {
            utf8_bytes: 0,
            num_images: 0,
            tools_enabled: false,
            web_search_enabled: false,
            code_interpreter_enabled: true,
        };
        let result = estimate_tokens(&input, &default_budgets());

        // base = 100, with margin = 110, + CI 1000
        assert_eq!(result.estimated_input_tokens, 110 + 1000);
    }

    #[test]
    fn safety_margin_applies_correctly() {
        // Margin is applied via multiply-first integer math: base * (100 + pct) / 100
        let budgets = EstimationBudgets {
            safety_margin_pct: 10,
            ..default_budgets()
        };
        let input = EstimationInput {
            utf8_bytes: 400,
            num_images: 0,
            tools_enabled: false,
            web_search_enabled: false,
            code_interpreter_enabled: false,
        };
        let result = estimate_tokens(&input, &budgets);

        // base = ceil(400/4) + 100 = 200
        // margin: ceil(200 * 110 / 100) = ceil(22000/100) = 220
        assert!(result.estimated_input_tokens > 200);
        assert_eq!(result.estimated_input_tokens, 220);
    }
}
