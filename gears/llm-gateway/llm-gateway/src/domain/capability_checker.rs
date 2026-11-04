//! Capability Checker (DESIGN §3.2): capability, parameter-policy,
//! structured-output-schema and request-limit checks, producing the effective
//! request and `ignored_parameters`. Runs after tool resolution, so the
//! request it receives only holds `function`, `cf_gears:*` and provider tools.
//!
//! # Contract (all checks complete before the broker and the plugin)
//!
//! Capabilities (error `capability_not_supported`, naming the capability):
//! - `supported_api` lacks `Completion` -> `completion`
//! - an `input_image` part -> `vision`; an `input_file` part -> `file_input`
//! - any function tool -> `function_calling`
//! - `cf_gears:web_search` -> `web_search` (model capability enabled, not
//!   disabled, AND the plugin reports `web_search`)
//! - `reasoning` with `effort` set -> `reasoning` (model `reasoning.effort`
//!   enabled and not disabled)
//! - `text.format = json_schema` -> `response_schema`
//!
//! A capability listed in `disabled_capabilities` is not supported.
//!
//! Parameter policy:
//! 1. A generic parameter the rules do not support (see
//!    [`GENERIC_PARAMETERS`](super::provider_rules::GENERIC_PARAMETERS)) and
//!    that the request sets: `unsupported_parameter` naming it, unless
//!    `drop_unsupported_params` is true, then it is dropped and listed.
//! 2. The model's `default_parameters` fill values the request omits (a
//!    default for a parameter the rules do not support is not applied).
//! 3. A request value for a parameter with a model default: kept when
//!    `allow_parameter_override` is true; when false the default is used and
//!    the parameter is listed in `ignored_parameters`.
//! 4. A cache-control hint (`prompt_cache_key`) the rules do not support is
//!    always dropped and listed, never an error.
//! 5. `extra_fields` keys must be listed in the model's `allow_extra_params`,
//!    else `unsupported_parameter` naming the key (or dropped and listed with
//!    `drop_unsupported_params`). The listed name is the bare key.
//!
//! Tools: `tool_choice` naming a function absent from the function tools ->
//! `validation_error` (param `tool_choice`); a provider tool `<prefix>:<tool>`
//! whose prefix differs from the plugin `tool_namespace` -> `validation_error`
//! (param `tools`).
//!
//! Structured output: a schema construct the rules cannot express ->
//! `unsupported_schema` naming the construct.
//!
//! Limits (`validation_error`): `max_input_items` (param `input`),
//! `max_tools` (param `tools`), `max_schema_bytes` for each function tool
//! `parameters` (param `tools`) and for the `text.format` schema (param
//! `text`), measured in serialized bytes.
//!
//! `ignored_parameters` is sorted ascending and deduplicated.
//!
//! # Embeddings contract ([`CapabilityChecker::check_embedding`])
//!
//! - `supported_api` lacks `Embedding` -> `capability_not_supported`
//!   (`embeddings`).
//! - empty input (`""`, `[]`) -> `validation_error` (param `input`); more
//!   than `max_embedding_inputs` texts; a batch holding an empty text is
//!   empty input too (whitespace-only texts are allowed) -> `validation_error` (param `input`).
//! - `dimensions` the rules do not support -> `unsupported_parameter`
//!   (`dimensions`), even with `drop_unsupported_params` (never dropped).
//! - `input_type` the rules do not support (`supports_parameter`): error, or
//!   dropped and listed with `drop_unsupported_params`.
//! - `encoding_format` is never checked: the plugin always honors it.
//! - `extra_fields` keys must be in `allow_extra_params`, else error (or
//!   dropped and listed), as for responses.
//! - the model's `default_parameters` are generation defaults and are not
//!   applied to embeddings; no override policy applies.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use llm_gateway_sdk::models::items::InputContent;
use llm_gateway_sdk::{
    CreateResponseBody, EmbeddingInput, EmbeddingRequest, InputContentPart, InputItem,
    LlmGatewayError, NamedToolChoice, ResponseInput, TextFormatKind, Tool, ToolChoice,
};
use model_registry_sdk::{ModelInfoV1, SupportedApi};
use serde::Serialize;
use serde::de::DeserializeOwned;
use toolkit_macros::domain_model;

use super::ports::ResolvedPlugin;
use super::provider_rules::{INPUT_TYPE, PROMPT_CACHE_KEY, ProviderRules, ProviderRulesRegistry};
use crate::config::LimitsConfig;

/// Outcome of the checks: the request to dispatch and the parameters that
/// were not applied.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveRequest {
    /// The request to hand to the plugin (defaults applied, dropped
    /// parameters removed).
    pub body: CreateResponseBody,
    /// Dropped and overridden parameters, sorted ascending, no duplicates.
    pub ignored_parameters: Vec<String>,
}

/// Outcome of the embedding checks: the request to dispatch and the
/// parameters that were dropped.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveEmbedding {
    /// The request to hand to the plugin (dropped parameters removed).
    pub request: EmbeddingRequest,
    /// Dropped parameters, sorted ascending, no duplicates.
    pub ignored_parameters: Vec<String>,
}

/// Runs the Capability Checker checks.
#[domain_model]
pub struct CapabilityChecker {
    limits: LimitsConfig,
    rules: Arc<ProviderRulesRegistry>,
}

impl CapabilityChecker {
    /// Checker with the request limits of `limits` and provider `rules`.
    #[must_use]
    pub fn new(limits: &LimitsConfig, rules: Arc<ProviderRulesRegistry>) -> Self {
        Self {
            limits: limits.clone(),
            rules,
        }
    }

    /// Run every check on the tool-resolved `body` for `model` served by
    /// `plugin`; see the module docs for the contract.
    ///
    /// # Errors
    ///
    /// `capability_not_supported`, `unsupported_parameter`,
    /// `unsupported_schema` or `validation_error`.
    pub fn check(
        &self,
        model: &ModelInfoV1,
        plugin: &ResolvedPlugin,
        body: CreateResponseBody,
    ) -> Result<EffectiveRequest, LlmGatewayError> {
        let rules = self.rules.rules_for(model.gts_type.as_ref());
        self.check_limits(&body)?;
        check_capabilities(model, plugin, &body)?;

        let mut body = body;
        let mut policy = ParameterPolicy {
            rules: rules.as_ref(),
            model,
            drop_unsupported: body.drop_unsupported_params.unwrap_or(false),
            ignored: Vec::new(),
        };
        policy.apply(&mut body)?;
        let mut ignored_parameters = policy.ignored;

        check_tools(plugin, &body)?;
        check_output_schema(rules.as_ref(), model, &body)?;

        ignored_parameters.sort();
        ignored_parameters.dedup();
        Ok(EffectiveRequest {
            body,
            ignored_parameters,
        })
    }

    /// Run the embedding checks on `request` for `model`; see the module
    /// docs ("Embeddings") for the contract.
    ///
    /// # Errors
    ///
    /// `capability_not_supported`, `unsupported_parameter` or
    /// `validation_error`.
    pub fn check_embedding(
        &self,
        model: &ModelInfoV1,
        request: EmbeddingRequest,
    ) -> Result<EffectiveEmbedding, LlmGatewayError> {
        require(
            model.supported_api.contains(&SupportedApi::Embedding),
            "embeddings",
        )?;
        self.check_embedding_input(&request.input)?;

        let rules = self.rules.rules_for(model.gts_type.as_ref());
        if let Some(dimensions) = request.dimensions {
            // Never dropped, whatever `drop_unsupported_params` says.
            let exceeds_model = model
                .context_window
                .output_vector_size
                .is_some_and(|size| dimensions > size);
            // Zero dimensions can never be honored exactly.
            if dimensions == 0 || exceeds_model || !rules.supports_dimensions(model, dimensions) {
                return Err(LlmGatewayError::unsupported_parameter("dimensions"));
            }
        }

        let mut request = request;
        let mut policy = ParameterPolicy {
            rules: rules.as_ref(),
            model,
            drop_unsupported: request.drop_unsupported_params.unwrap_or(false),
            ignored: Vec::new(),
        };
        policy.parameter(INPUT_TYPE, &mut request.input_type, None)?;
        policy.extra_fields(&mut request.extra_fields)?;

        let mut ignored_parameters = policy.ignored;
        ignored_parameters.sort();
        ignored_parameters.dedup();
        Ok(EffectiveEmbedding {
            request,
            ignored_parameters,
        })
    }

    fn check_embedding_input(&self, input: &EmbeddingInput) -> Result<(), LlmGatewayError> {
        let texts: &[String] = match input {
            EmbeddingInput::Single(text) => std::slice::from_ref(text),
            EmbeddingInput::Many(texts) => texts,
        };
        if texts.is_empty() || texts.iter().any(String::is_empty) {
            return Err(LlmGatewayError::validation_param(
                "input must not be empty",
                "input",
            ));
        }
        if texts.len() > self.limits.max_embedding_inputs {
            return Err(LlmGatewayError::validation_param(
                "too many embedding inputs",
                "input",
            ));
        }
        Ok(())
    }

    fn check_limits(&self, body: &CreateResponseBody) -> Result<(), LlmGatewayError> {
        if let Some(ResponseInput::Items(items)) = &body.input
            && items.len() > self.limits.max_input_items
        {
            return Err(LlmGatewayError::validation_param(
                "too many input items",
                "input",
            ));
        }
        let tools = body.tools.as_deref().unwrap_or_default();
        if tools.len() > self.limits.max_tools {
            return Err(LlmGatewayError::validation_param("too many tools", "tools"));
        }
        for tool in tools {
            if let Tool::Function(function) = tool
                && let Some(parameters) = &function.parameters
                && self.exceeds_schema_limit(parameters)
            {
                return Err(LlmGatewayError::validation_param(
                    "tool schema is too large",
                    "tools",
                ));
            }
        }
        if let Some(schema) = output_schema(body)
            && self.exceeds_schema_limit(schema)
        {
            return Err(LlmGatewayError::validation_param(
                "output schema is too large",
                "text",
            ));
        }
        Ok(())
    }

    fn exceeds_schema_limit(&self, schema: &serde_json::Value) -> bool {
        // Serializing a `Value` cannot fail; an impossible failure counts as
        // oversized rather than slipping through.
        serde_json::to_vec(schema).map_or(true, |bytes| bytes.len() > self.limits.max_schema_bytes)
    }
}

fn output_schema(body: &CreateResponseBody) -> Option<&serde_json::Value> {
    match &body.text.as_ref()?.format {
        TextFormatKind::JsonSchema { schema, .. } => schema.as_ref(),
        _ => None,
    }
}

fn wants_json_schema(body: &CreateResponseBody) -> bool {
    matches!(
        body.text.as_ref().map(|text| &text.format),
        Some(TextFormatKind::JsonSchema { .. })
    )
}

fn content_parts(body: &CreateResponseBody) -> impl Iterator<Item = &InputContentPart> {
    let items: &[InputItem] = match &body.input {
        Some(ResponseInput::Items(items)) => items,
        _ => &[],
    };
    items
        .iter()
        .filter_map(|item| match item {
            InputItem::Message(message) => match &message.content {
                InputContent::Parts(parts) => Some(parts.iter()),
                InputContent::Text(_) => None,
            },
            _ => None,
        })
        .flatten()
}

fn require(supported: bool, capability: &str) -> Result<(), LlmGatewayError> {
    if supported {
        Ok(())
    } else {
        Err(LlmGatewayError::capability_not_supported(capability))
    }
}

fn check_capabilities(
    model: &ModelInfoV1,
    plugin: &ResolvedPlugin,
    body: &CreateResponseBody,
) -> Result<(), LlmGatewayError> {
    let caps = &model.capabilities;
    let disabled = &model.disabled_capabilities;

    require(
        model.supported_api.contains(&SupportedApi::Completion),
        "completion",
    )?;

    let parts: Vec<&InputContentPart> = content_parts(body).collect();
    if parts
        .iter()
        .any(|part| matches!(part, InputContentPart::InputImage(_)))
    {
        require(caps.vision.enabled && !disabled.vision.disabled, "vision")?;
    }
    if parts
        .iter()
        .any(|part| matches!(part, InputContentPart::InputFile(_)))
    {
        require(
            caps.file_input.enabled && !disabled.file_input.disabled,
            "file_input",
        )?;
    }

    let tools = body.tools.as_deref().unwrap_or_default();
    if tools.iter().any(|tool| matches!(tool, Tool::Function(_))) {
        require(
            caps.function_calling && !disabled.function_calling,
            "function_calling",
        )?;
    }
    if tools.iter().any(|tool| matches!(tool, Tool::WebSearch(_))) {
        require(
            caps.web_search.enabled
                && !disabled.web_search.disabled
                && plugin.capabilities().web_search,
            "web_search",
        )?;
    }

    if body
        .reasoning
        .as_ref()
        .is_some_and(|reasoning| reasoning.effort.is_some())
    {
        require(
            caps.reasoning.effort && !disabled.reasoning.effort,
            "reasoning",
        )?;
    }

    if wants_json_schema(body) {
        require(
            caps.response_schema && !disabled.response_schema,
            "response_schema",
        )?;
    }
    Ok(())
}

fn check_tools(plugin: &ResolvedPlugin, body: &CreateResponseBody) -> Result<(), LlmGatewayError> {
    let tools = body.tools.as_deref().unwrap_or_default();
    let mut functions: HashSet<&str> = HashSet::new();
    for tool in tools {
        match tool {
            Tool::Function(function) => {
                if !functions.insert(function.name.as_str()) {
                    return Err(LlmGatewayError::validation_param(
                        format!("duplicate function tool name: {}", function.name),
                        "tools",
                    ));
                }
            }
            Tool::Other(extension) => {
                let namespace = extension
                    .kind()
                    .and_then(|kind| kind.split_once(':'))
                    .map(|(prefix, _)| prefix);
                if namespace != Some(plugin.tool_namespace.as_str())
                    || plugin.tool_namespace.is_empty()
                {
                    return Err(LlmGatewayError::validation_param(
                        "tool type is not served by the provider of the model",
                        "tools",
                    ));
                }
            }
            Tool::Reference(_) | Tool::InlineGts(_) => {
                return Err(LlmGatewayError::internal(
                    "unresolved tool reached the capability checker",
                ));
            }
            _ => {}
        }
    }
    if let Some(ToolChoice::Named(NamedToolChoice::Function { name })) = &body.tool_choice
        && !functions.contains(name.as_str())
    {
        return Err(LlmGatewayError::validation_param(
            "tool_choice names a function that is not among the tools",
            "tool_choice",
        ));
    }
    Ok(())
}

fn check_output_schema(
    rules: &dyn ProviderRules,
    model: &ModelInfoV1,
    body: &CreateResponseBody,
) -> Result<(), LlmGatewayError> {
    match output_schema(body).and_then(|schema| rules.unsupported_schema_construct(model, schema)) {
        Some(construct) => Err(LlmGatewayError::unsupported_schema(construct)),
        None => Ok(()),
    }
}

/// Re-encode a registry-side value as its gateway-side twin (same wire shape,
/// distinct Rust types).
fn convert<A: Serialize, B: DeserializeOwned>(value: Option<&A>) -> Option<B> {
    serde_json::from_value(serde_json::to_value(value?).ok()?).ok()
}

/// Generic-parameter policy of one request (module docs, "Parameter policy").
struct ParameterPolicy<'a> {
    rules: &'a dyn ProviderRules,
    model: &'a ModelInfoV1,
    drop_unsupported: bool,
    ignored: Vec<String>,
}

impl ParameterPolicy<'_> {
    /// Apply rules, defaults, override policy, cache hint and extra fields.
    fn apply(&mut self, body: &mut CreateResponseBody) -> Result<(), LlmGatewayError> {
        let defaults = &self.model.default_parameters;
        self.parameter("temperature", &mut body.temperature, defaults.temperature)?;
        self.parameter("top_p", &mut body.top_p, defaults.top_p)?;
        self.parameter(
            "max_output_tokens",
            &mut body.max_output_tokens,
            defaults.max_output_tokens,
        )?;
        let has_tools = body.tools.as_ref().is_some_and(|tools| !tools.is_empty());
        self.parameter(
            "max_tool_calls",
            &mut body.max_tool_calls,
            defaults.max_tool_calls.filter(|_| has_tools),
        )?;
        self.parameter("seed", &mut body.seed, None)?;
        self.parameter(
            "presence_penalty",
            &mut body.presence_penalty,
            defaults.presence_penalty,
        )?;
        self.parameter(
            "frequency_penalty",
            &mut body.frequency_penalty,
            defaults.frequency_penalty,
        )?;
        self.parameter(
            "top_logprobs",
            &mut body.top_logprobs,
            defaults.top_logprobs,
        )?;
        self.parameter(
            "truncation",
            &mut body.truncation,
            convert(defaults.truncation.as_ref()),
        )?;
        self.parameter(
            "service_tier",
            &mut body.service_tier,
            convert(defaults.service_tier.as_ref()),
        )?;
        self.parameter(
            "parallel_tool_calls",
            &mut body.parallel_tool_calls,
            defaults.parallel_tool_calls.filter(|_| has_tools),
        )?;
        self.parameter(
            "tool_choice",
            &mut body.tool_choice,
            convert(defaults.tool_choice.as_ref()).filter(|_| has_tools),
        )?;

        if body.prompt_cache_key.is_some()
            && !self
                .rules
                .supports_cache_control(self.model, PROMPT_CACHE_KEY)
        {
            body.prompt_cache_key = None;
            self.ignored.push(PROMPT_CACHE_KEY.to_owned());
        }

        self.extra_fields(&mut body.extra_fields)
    }

    fn parameter<T: PartialEq>(
        &mut self,
        name: &str,
        request: &mut Option<T>,
        default: Option<T>,
    ) -> Result<(), LlmGatewayError> {
        if !self.rules.supports_parameter(self.model, name) {
            if request.is_some() {
                if !self.drop_unsupported {
                    return Err(LlmGatewayError::unsupported_parameter(name));
                }
                *request = None;
                self.ignored.push(name.to_owned());
            }
            return Ok(());
        }
        let Some(default) = default else {
            return Ok(());
        };
        match request {
            None => *request = Some(default),
            Some(value) if !self.model.allow_parameter_override => {
                if *value != default {
                    self.ignored.push(name.to_owned());
                }
                *value = default;
            }
            Some(_) => {}
        }
        Ok(())
    }

    fn extra_fields(
        &mut self,
        fields: &mut Option<HashMap<String, serde_json::Value>>,
    ) -> Result<(), LlmGatewayError> {
        let Some(extras) = fields.as_mut() else {
            return Ok(());
        };
        let mut unlisted: Vec<String> = extras
            .keys()
            .filter(|key| !self.model.allow_extra_params.contains(key))
            .cloned()
            .collect();
        unlisted.sort();
        if let Some(first) = unlisted.first()
            && !self.drop_unsupported
        {
            return Err(LlmGatewayError::unsupported_parameter(first.as_str()));
        }
        for key in unlisted {
            extras.remove(&key);
            self.ignored.push(key);
        }
        if extras.is_empty() {
            *fields = None;
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "capability_checker_tests.rs"]
mod capability_checker_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "capability_checker_embedding_tests.rs"]
mod capability_checker_embedding_tests;
