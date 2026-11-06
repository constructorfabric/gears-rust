//! Quota pricing arithmetic (DESIGN §3.2 Quota Guard, "Amount").
//!
//! One price unit (`input_per_1k_micro` and friends) is fixed at
//! 0.000001 AI credit. Credits convert to the metric's integer unit with
//! exact decimal and integer arithmetic and a single rounding up at the end.

use llm_gateway_sdk::models::content::InputContentPart;
use llm_gateway_sdk::models::items::{
    InputContent, InputItem, ReasoningContentPart, ReasoningSummaryPart,
};
use llm_gateway_sdk::{CreateResponseBody, EmbeddingInput, EmbeddingRequest, ResponseInput, Usage};
use model_registry_sdk::ModelInfoV1;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive as _;
use toolkit_macros::domain_model;

/// Parsed `provider_settings.cost` of a model: micro-credits per 1000 tokens.
/// `None` is "absent or unparseable".
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelPricing {
    /// `input_per_1k_micro`.
    pub input_per_1k_micro: Option<u64>,
    /// `output_per_1k_micro`.
    pub output_per_1k_micro: Option<u64>,
}

/// Pricing that passed the pre-check for one operation: every rate the
/// operation requires is present. Embeddings carry `output_per_1k_micro = 0`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pricing {
    /// Micro-credits per 1000 input tokens.
    pub input_per_1k_micro: u64,
    /// Micro-credits per 1000 output tokens.
    pub output_per_1k_micro: u64,
}

impl Pricing {
    /// All required rates are 0: the request reserves nothing.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.input_per_1k_micro == 0 && self.output_per_1k_micro == 0
    }
}

/// Token estimate of `bytes` UTF-8 bytes of text: bytes / 4, rounded up (the
/// core has no tokenizer).
#[must_use]
pub fn tokens_from_bytes(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}

/// Length of `value` serialized as JSON, counted without building the string.
fn json_bytes<T: serde::Serialize>(value: &T) -> u64 {
    let mut sink = CountingSink(0);
    // A serialization failure (not expected for SDK types) counts what was
    // written so far.
    serde_json::to_writer(&mut sink, value).ok();
    sink.0
}

/// `io::Write` sink that only counts bytes.
struct CountingSink(u64);

impl std::io::Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// UTF-8 bytes of the text of an input content value (media adds none;
/// extension parts count their serialized JSON).
fn content_bytes(content: &InputContent) -> u64 {
    match content {
        InputContent::Text(text) => text.len() as u64,
        InputContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                InputContentPart::InputText(text) => text.text.len() as u64,
                InputContentPart::Other(other) => json_bytes(other),
                _ => 0,
            })
            .sum(),
    }
}

/// UTF-8 bytes of the text of one input item.
fn item_bytes(item: &InputItem) -> u64 {
    match item {
        InputItem::Message(message) => content_bytes(&message.content),
        InputItem::FunctionCall(call) => call.arguments.len() as u64,
        InputItem::FunctionCallOutput(output) => content_bytes(&output.output),
        InputItem::Reasoning(reasoning) => {
            let summary: u64 = reasoning
                .summary
                .iter()
                .map(|ReasoningSummaryPart::SummaryText { text }| text.len() as u64)
                .sum();
            let content: u64 = reasoning
                .content
                .iter()
                .flatten()
                .map(|ReasoningContentPart::ReasoningText { text }| text.len() as u64)
                .sum();
            let encrypted = reasoning
                .encrypted_content
                .as_ref()
                .map_or(0, |s| s.len() as u64);
            summary.saturating_add(content).saturating_add(encrypted)
        }
        InputItem::Other(other) => json_bytes(other),
        InputItem::ItemReference(_) => 0,
    }
}

/// Estimated input tokens of a create-response request: UTF-8 bytes of
/// `instructions`, text input, text parts of message items, function-call
/// arguments and outputs, reasoning items (summary, content, encrypted
/// content), the serialized extension items and parts, and the serialized
/// tool definitions. Media parts add none.
#[must_use]
pub fn estimate_input_tokens(body: &CreateResponseBody) -> u64 {
    let mut bytes: u64 = body.instructions.as_ref().map_or(0, |s| s.len() as u64);
    bytes = bytes.saturating_add(match &body.input {
        Some(ResponseInput::Text(text)) => text.len() as u64,
        Some(ResponseInput::Items(items)) => items.iter().map(item_bytes).sum(),
        None => 0,
    });
    if let Some(tools) = body.tools.as_ref().filter(|tools| !tools.is_empty()) {
        bytes = bytes.saturating_add(json_bytes(tools));
    }
    tokens_from_bytes(bytes)
}

/// Estimated input tokens of an embeddings request (all input texts).
#[must_use]
pub fn estimate_embedding_tokens(req: &EmbeddingRequest) -> u64 {
    let bytes: u64 = match &req.input {
        EmbeddingInput::Single(text) => text.len() as u64,
        EmbeddingInput::Many(texts) => texts.iter().map(|t| t.len() as u64).sum(),
    };
    tokens_from_bytes(bytes)
}

/// Output allowance: the request's `max_output_tokens`, else the model's
/// output limit, else `fallback`.
#[must_use]
pub fn max_output_tokens(body: &CreateResponseBody, model: &ModelInfoV1, fallback: u64) -> u64 {
    body.max_output_tokens
        .or(model.context_window.max_output_tokens)
        .map_or(fallback, u64::from)
}

/// One price unit in AI credits: 0.000001 (micro-credit). Fixed, shared by the
/// core and every plugin; not configuration.
const PRICE_UNIT_DIVISOR: u64 = 1_000_000;
/// Rates are per 1000 tokens.
const TOKENS_PER_RATE: u64 = 1_000;

/// Cost in AI credits of `input_tokens` and `output_tokens` at `pricing`.
/// Exact decimal arithmetic; saturates at `Decimal::MAX`.
#[must_use]
pub fn credits(pricing: &Pricing, input_tokens: u64, output_tokens: u64) -> Decimal {
    let part = |tokens: u64, rate: u64| Decimal::from(tokens).checked_mul(Decimal::from(rate));
    let price_units_times_1k = match (
        part(input_tokens, pricing.input_per_1k_micro),
        part(output_tokens, pricing.output_per_1k_micro),
    ) {
        (Some(input), Some(output)) => input.checked_add(output),
        _ => None,
    };
    price_units_times_1k
        .and_then(|sum| sum.checked_div(Decimal::from(TOKENS_PER_RATE * PRICE_UNIT_DIVISOR)))
        .unwrap_or(Decimal::MAX)
}

/// Metric units of `credits` (`units_per_credit` per credit), rounded up once.
/// Negative amounts are 0; amounts above `i64::MAX` saturate.
#[must_use]
pub fn to_units(credits: Decimal, units_per_credit: u64) -> i64 {
    if credits <= Decimal::ZERO {
        return 0;
    }
    credits
        .checked_mul(Decimal::from(units_per_credit))
        .map_or(i64::MAX, |units| units.ceil().to_i64().unwrap_or(i64::MAX))
}

/// Core estimate for requests without final provider usage.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct UsageEstimator {
    /// Estimated input tokens of the request.
    pub input_tokens: u64,
    /// Rates of the model.
    pub pricing: Pricing,
}

impl UsageEstimator {
    /// Estimated usage: input tokens plus the higher of the output relayed
    /// so far (`output_bytes` as tokens) and the last partial provider usage;
    /// priced at `pricing`; `complete = false`, `estimated = true`.
    #[must_use]
    pub fn estimate(&self, output_bytes: u64, last_usage: Option<&Usage>) -> Usage {
        let relayed = tokens_from_bytes(output_bytes);
        let output_tokens = last_usage.map_or(relayed, |usage| relayed.max(usage.output_tokens));
        Usage::estimated(
            self.input_tokens,
            output_tokens,
            credits(&self.pricing, self.input_tokens, output_tokens),
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "pricing_tests.rs"]
mod pricing_tests;
