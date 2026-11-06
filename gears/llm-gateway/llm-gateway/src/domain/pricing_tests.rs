//! Quota pricing arithmetic (DESIGN §3.2 Quota Guard "Amount": bytes/4 input
//! estimate, output allowance, fixed price unit, one rounding up) and the
//! incomplete-usage estimator (§3.2 Usage Reporter table).

use llm_gateway_sdk::{CreateResponseBody, EmbeddingInput, EmbeddingRequest, Usage};
use rust_decimal::Decimal;
use serde_json::json;

use super::{
    Pricing, UsageEstimator, credits, estimate_embedding_tokens, estimate_input_tokens,
    max_output_tokens, to_units, tokens_from_bytes,
};
use crate::domain::test_support::{OPENAI_TYPE, sample_model_info_patched};

const fn rates(input: u64, output: u64) -> Pricing {
    Pricing {
        input_per_1k_micro: input,
        output_per_1k_micro: output,
    }
}

fn dec(mantissa: i64, scale: u32) -> Decimal {
    Decimal::new(mantissa, scale)
}

#[allow(clippy::needless_pass_by_value)] // call sites build the patch inline with `json!`
fn request(value: serde_json::Value) -> CreateResponseBody {
    let mut body = json!({ "model": "openai::gpt-4o" });
    for (key, field) in value.as_object().expect("object") {
        body[key] = field.clone();
    }
    serde_json::from_value(body).expect("body deserializes")
}

// ── bytes / 4 ───────────────────────────────────────────────────────────────

#[test]
fn tokens_are_utf8_bytes_over_four_rounded_up() {
    assert_eq!(tokens_from_bytes(0), 0);
    assert_eq!(tokens_from_bytes(1), 1);
    assert_eq!(tokens_from_bytes(4), 1);
    assert_eq!(tokens_from_bytes(5), 2);
    assert_eq!(tokens_from_bytes(400), 100);
}

#[test]
fn text_input_counts_its_utf8_bytes_not_its_chars() {
    // "\u{e9}\u{e9}\u{e9}\u{e9}" is 4 chars and 8 bytes: 2 tokens, not 1.
    assert_eq!(
        estimate_input_tokens(&request(json!({ "input": "\u{e9}\u{e9}\u{e9}\u{e9}" }))),
        2
    );
    assert_eq!(
        estimate_input_tokens(&request(json!({ "input": "hello" }))),
        2
    );
}

#[test]
fn instructions_and_input_are_summed_before_rounding() {
    // 2 + 2 = 4 bytes -> 1 token (rounding each part up would give 2).
    let both = request(json!({ "instructions": "ab", "input": "ab" }));
    assert_eq!(estimate_input_tokens(&both), 1);
    // 4 + 5 = 9 bytes -> 3 tokens.
    let nine = request(json!({ "instructions": "abcd", "input": "hello" }));
    assert_eq!(estimate_input_tokens(&nine), 3);
}

#[test]
fn message_text_parts_function_calls_and_outputs_count() {
    let body = request(json!({
        "input": [
            { "type": "message", "role": "user", "content": "abcd" },
            { "type": "message", "role": "user",
              "content": [{ "type": "input_text", "text": "abcd" }] },
            { "type": "function_call", "call_id": "c1", "name": "f", "arguments": "abcd" },
            { "type": "function_call_output", "call_id": "c1", "output": "abcd" }
        ]
    }));
    assert_eq!(estimate_input_tokens(&body), 4, "4 x 4 bytes");
}

#[test]
fn media_parts_add_no_text_bytes() {
    let body = request(json!({
        "input": [{ "type": "message", "role": "user", "content": [
            { "type": "input_text", "text": "abcd" },
            { "type": "input_image", "image_url": "https://example.test/a-very-long-image-url.png" }
        ] }]
    }));
    assert_eq!(estimate_input_tokens(&body), 1);
}

#[test]
fn reasoning_items_count_summary_content_and_encrypted_bytes() {
    let body = request(json!({
        "input": [{ "type": "reasoning", "id": "rs_1",
            "summary": [{ "type": "summary_text", "text": "abcd" },
                        { "type": "summary_text", "text": "abcd" }],
            "content": [{ "type": "reasoning_text", "text": "abcdefgh" }],
            "encrypted_content": "abcdefgh" }]
    }));
    // 4 + 4 + 8 + 8 = 24 bytes (id and structure are not text).
    assert_eq!(estimate_input_tokens(&body), 6);
    let bare = request(json!({
        "input": [{ "type": "reasoning", "summary": [] }]
    }));
    assert_eq!(estimate_input_tokens(&bare), 0);
}

#[test]
fn extension_items_and_parts_count_their_serialized_json() {
    let item = json!({ "type": "vendor:thing", "payload": "abcdefgh" });
    let item_bytes = serde_json::to_string(&item).expect("serializes").len() as u64;
    let body = request(json!({ "input": [item] }));
    assert_eq!(estimate_input_tokens(&body), tokens_from_bytes(item_bytes));
    assert!(item_bytes > 8);

    let part = json!({ "type": "vendor:part", "data": "abcdefgh" });
    let part_bytes = serde_json::to_string(&part).expect("serializes").len() as u64;
    let body = request(json!({
        "input": [{ "type": "message", "role": "user", "content": [part] }]
    }));
    assert_eq!(estimate_input_tokens(&body), tokens_from_bytes(part_bytes));
}

#[test]
fn tool_definitions_count_as_their_serialized_json() {
    let body = request(json!({
        "tools": [{ "type": "function", "name": "lookup",
                    "parameters": { "type": "object", "properties": {} } }]
    }));
    let bytes = serde_json::to_string(body.tools.as_ref().expect("tools"))
        .expect("tools serialize")
        .len() as u64;
    assert_eq!(estimate_input_tokens(&body), tokens_from_bytes(bytes));
    assert!(estimate_input_tokens(&body) > 0);
}

#[test]
fn an_empty_request_estimates_zero_input_tokens() {
    assert_eq!(estimate_input_tokens(&request(json!({}))), 0);
}

#[test]
fn embedding_estimate_sums_every_input_text() {
    let single = EmbeddingRequest::new("m", EmbeddingInput::Single("abcdefgh".to_owned()));
    assert_eq!(estimate_embedding_tokens(&single), 2);
    let many = EmbeddingRequest::new(
        "m",
        EmbeddingInput::Many(vec![
            "abcd".to_owned(),
            "abcd".to_owned(),
            "abcd".to_owned(),
        ]),
    );
    assert_eq!(estimate_embedding_tokens(&many), 3, "12 bytes");
}

// ── output allowance ────────────────────────────────────────────────────────

#[test]
fn output_allowance_prefers_the_request_limit() {
    let model = sample_model_info_patched(OPENAI_TYPE, |_| {});
    let body = request(json!({ "max_output_tokens": 100 }));
    assert_eq!(max_output_tokens(&body, &model, 999), 100);
}

#[test]
fn output_allowance_falls_back_to_the_model_output_limit() {
    // The sample model's `context_window.max_output_tokens` is 4096.
    let model = sample_model_info_patched(OPENAI_TYPE, |_| {});
    assert_eq!(max_output_tokens(&request(json!({})), &model, 999), 4096);
}

#[test]
fn output_allowance_falls_back_to_the_configured_fallback_last() {
    let model = sample_model_info_patched(OPENAI_TYPE, |v| {
        v["context_window"]["max_output_tokens"] = serde_json::Value::Null;
    });
    assert_eq!(max_output_tokens(&request(json!({})), &model, 999), 999);
}

// ── price conversion ────────────────────────────────────────────────────────

#[test]
fn credits_are_price_units_of_one_millionth_credit_per_thousand_tokens() {
    // 1000 input tokens at 3 price units per 1K = 3 price units = 0.000003.
    assert_eq!(credits(&rates(3, 0), 1000, 0), dec(3, 6));
    // 500 input at 2000/1K = 1000 units; 250 output at 4000/1K = 1000 units.
    assert_eq!(credits(&rates(2000, 4000), 500, 250), dec(2000, 6));
}

#[test]
fn credits_are_exact_for_fractional_results() {
    // 1 token at 3 per 1K = 0.003 price units = 3e-9 credits, not rounded.
    assert_eq!(credits(&rates(3, 0), 1, 0), dec(3, 9));
}

#[test]
fn zero_rates_or_zero_tokens_cost_nothing() {
    assert_eq!(credits(&rates(0, 0), 123, 456), Decimal::ZERO);
    assert_eq!(credits(&rates(5, 7), 0, 0), Decimal::ZERO);
}

#[test]
fn three_price_units_convert_to_exactly_three_micro_credits_not_four() {
    // 3 x 1e-6 x 1e6 is 3.0000000000000004 in binary floating point, which
    // rounds up to 4; exact decimal arithmetic gives 3.
    let cost = credits(&rates(3, 0), 1000, 0);
    assert_eq!(to_units(cost, 1_000_000), 3);
}

#[test]
fn units_round_up_exactly_once_at_the_end() {
    assert_eq!(to_units(dec(3, 9), 1_000_000), 1, "0.003 units round up");
    assert_eq!(to_units(dec(2_000_001, 12), 1_000_000), 3, "2.000001 -> 3");
    assert_eq!(to_units(Decimal::ZERO, 1_000_000), 0);
    assert_eq!(to_units(dec(5, 0), 1_000_000), 5_000_000, "5 credits");
}

#[test]
fn units_follow_the_configured_units_per_credit() {
    assert_eq!(
        to_units(dec(5, 1), 1_000),
        500,
        "0.5 credit at 1000 per credit"
    );
    assert_eq!(
        to_units(dec(1, 6), 1_000),
        1,
        "1e-6 credit rounds up to 1 unit"
    );
}

#[test]
fn negative_amounts_are_zero_and_huge_amounts_saturate() {
    assert_eq!(to_units(dec(-5, 0), 1_000_000), 0);
    assert_eq!(to_units(Decimal::MAX, 1_000_000), i64::MAX);
}

#[test]
fn the_per_token_sum_is_not_rounded_per_part() {
    // Input 1 token at 1/1K = 1e-9; output 1 token at 1/1K = 1e-9. Rounding
    // each part up would give 2 units; the sum 2e-9 credits is 1 unit.
    let cost = credits(&rates(1, 1), 1, 1);
    assert_eq!(to_units(cost, 1_000_000), 1);
}

// ── pricing flags ───────────────────────────────────────────────────────────

#[test]
fn only_all_zero_rates_are_zero_pricing() {
    assert!(rates(0, 0).is_zero());
    assert!(!rates(1, 0).is_zero());
    assert!(!rates(0, 1).is_zero());
}

// ── incomplete-usage estimator ──────────────────────────────────────────────

fn estimator() -> UsageEstimator {
    UsageEstimator {
        input_tokens: 100,
        pricing: rates(1000, 2000),
    }
}

fn partial(output_tokens: u64) -> Usage {
    Usage::reported(7, output_tokens, Decimal::ZERO)
}

#[test]
fn estimate_is_flagged_incomplete_and_estimated() {
    let usage = estimator().estimate(0, None);
    assert!(!usage.complete);
    assert!(usage.estimated);
}

#[test]
fn estimate_prices_input_plus_relayed_output_bytes_over_four() {
    // 40 relayed bytes = 10 output tokens; (100 x 1000 + 10 x 2000) / 1000 =
    // 120 price units = 0.00012 credit.
    let usage = estimator().estimate(40, None);
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 10);
    assert_eq!(usage.cost, dec(120, 6));
}

#[test]
fn estimate_with_no_output_relayed_prices_the_input_only() {
    let usage = estimator().estimate(0, None);
    assert_eq!(usage.output_tokens, 0);
    assert_eq!(usage.cost, dec(100, 6));
}

#[test]
fn estimate_takes_the_higher_of_relayed_output_and_last_partial_usage() {
    let from_partial = estimator().estimate(40, Some(&partial(50)));
    assert_eq!(from_partial.output_tokens, 50, "partial usage is higher");
    let from_relayed = estimator().estimate(400, Some(&partial(50)));
    assert_eq!(from_relayed.output_tokens, 100, "relayed output is higher");
}

#[test]
fn estimate_prices_through_the_pricing_not_the_partial_usage_cost() {
    let mut last = partial(10);
    last.cost = dec(999, 0);
    let usage = estimator().estimate(0, Some(&last));
    assert_eq!(
        usage.cost,
        dec(120, 6),
        "100 in + 10 out, not the partial cost"
    );
}

#[test]
fn estimate_under_zero_pricing_costs_nothing() {
    let zero = UsageEstimator {
        input_tokens: 100,
        pricing: rates(0, 0),
    };
    assert_eq!(zero.estimate(400, None).cost, Decimal::ZERO);
}
