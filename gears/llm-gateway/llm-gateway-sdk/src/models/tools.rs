// Created: 2026-07-09 by Constructor Tech
//! Tool models — from `schemas/tools/`.
//!
//! Tools form a `type`-discriminated family, mapped here to a tagged enum.
//! `parameters` and inline schemas stay as `serde_json::Value` since they are
//! arbitrary JSON Schema.

use crate::models::extension::Extension;

// ---------------------------------------------------------------------------
// Tool
// ---------------------------------------------------------------------------

/// A tool definition, discriminated by `type`.
///
/// Core-owned tool types have named variants; any other `type` — a provider or
/// third-party plugin extension the core does not own — is preserved verbatim
/// in [`Tool::Other`] and forwarded without interpretation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum Tool {
    /// A caller-defined function tool.
    #[serde(rename = "function")]
    Function(FunctionTool),
    /// A reference to a tool schema in the Type Registry.
    #[serde(rename = "reference")]
    Reference(ToolReference),
    /// An inline GTS schema definition.
    #[serde(rename = "inline_gts")]
    InlineGts(ToolInlineGts),
    /// The provider-agnostic Gateway web search tool. The provider plugin maps
    /// it to the provider's native web search; the Gateway never runs searches
    /// itself (`cpt-cf-llm-gateway-fr-web-search-v1`). Named under the
    /// Gateway-owned `cf_gears:` prefix (ADR 0001).
    #[serde(rename = "cf_gears:web_search")]
    WebSearch(WebSearchTool),
    /// The built-in image-generation tool (Gears extension).
    #[serde(rename = "cf_gears:image_generation")]
    ImageGeneration(ImageGenerationTool),
    /// A tool `type` the core does not own, preserved verbatim. Provider-specific
    /// tools are namespaced by provider (`<provider>:<tool>`, e.g.
    /// `openai:shell`) and forwarded to the matching plugin
    /// (`cpt-cf-llm-gateway-fr-provider-tools-v1`).
    #[serde(untagged)]
    Other(Extension),
}

/// Gateway web search tool (`cf_gears:web_search`). Results and citations come
/// back as `cf_gears:web_search_call` output items and `url_citation`
/// annotations.
///
/// The generic option is [`WebSearchTool::allowed_domains`]. Any other fields
/// (e.g. `search_context_size`) are kept in [`WebSearchTool::options`] and
/// handed to the plugin, which maps those its provider supports. Accepting them
/// here keeps the tool a `WebSearch` value: if they made decoding fail, the
/// tool would fall through to [`Tool::Other`] and skip the web search
/// capability check.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[non_exhaustive]
pub struct WebSearchTool {
    /// Domains the search is restricted to (e.g. `example.com`, which also
    /// covers its subdomains). `None` means no restriction.
    ///
    /// This narrows what the model can read, so it is never dropped: when the
    /// provider cannot restrict search by domain, the request is rejected with
    /// `unsupported_parameter`, even when `drop_unsupported_params` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,

    /// Tool options other than `type` and `allowed_domains`, preserved
    /// verbatim.
    #[serde(flatten)]
    pub options: serde_json::Map<String, serde_json::Value>,
}

impl WebSearchTool {
    /// Restrict the search to these domains.
    #[must_use]
    pub fn with_allowed_domains(mut self, domains: Vec<String>) -> Self {
        self.allowed_domains = Some(domains);
        self
    }

    /// Add a provider-specific option, forwarded to the plugin verbatim.
    #[must_use]
    pub fn with_option(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.options.insert(key.into(), value);
        self
    }
}

/// Function tool.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FunctionTool {
    /// Function name (schema pattern `^[a-zA-Z0-9_-]+$`, 1..=64 chars).
    pub name: String,
    /// Function description for the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the function parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    /// Strict schema enforcement for function arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// Tool reference.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ToolReference {
    /// GTS identifier of the tool schema in the Type Registry.
    pub schema_id: String,
}

/// Inline GTS tool schema.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ToolInlineGts {
    /// Inline schema definition.
    pub schema: Schema,
}

/// JSON Schema wrapper.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Schema {
    /// JSON Schema definition.
    pub json_schema: serde_json::Value,
}

/// Image-generation tool (Gears extension).
///
/// All fields default to null, deferring to the provider/gateway default.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ImageGenerationTool {
    /// Output image aspect ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<AspectRatio>,
    /// Resolution in megapixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// Generation quality level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<ImageQuality>,
    /// Image file format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<ImageOutputFormat>,
    /// Compression level 0..=100 (applies to jpeg).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_compression: Option<u8>,
    /// How the generated image is returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ImageResponseFormat>,
}

/// Output image aspect ratio.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub enum AspectRatio {
    #[serde(rename = "1:1")]
    Square,
    #[serde(rename = "2:3")]
    TwoByThree,
    #[serde(rename = "3:2")]
    ThreeByTwo,
    #[serde(rename = "3:4")]
    ThreeByFour,
    #[serde(rename = "4:3")]
    FourByThree,
    #[serde(rename = "9:16")]
    NineBySixteen,
    #[serde(rename = "16:9")]
    SixteenByNine,
}

/// Output resolution in megapixels.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub enum Resolution {
    #[serde(rename = "0.5")]
    Half,
    #[serde(rename = "1")]
    One,
    #[serde(rename = "2")]
    Two,
    #[serde(rename = "4")]
    Four,
}

/// Image generation quality level.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ImageQuality {
    Low,
    Medium,
    High,
    Auto,
}

/// Generated image file format.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ImageOutputFormat {
    Png,
    Jpeg,
    Webp,
}

/// How a generated image is returned.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ImageResponseFormat {
    Base64,
    Url,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Extra fields must not push the Gateway tool into `Other`, where it would
    // bypass the web search capability check.
    #[test]
    fn web_search_tool_with_options_stays_web_search() {
        let wire = serde_json::json!({
            "type": "cf_gears:web_search",
            "search_context_size": "high"
        });
        let tool: Tool = serde_json::from_value(wire.clone()).unwrap();
        let Tool::WebSearch(web_search) = &tool else {
            panic!("expected WebSearch, got {tool:?}");
        };
        assert_eq!(
            web_search.options.get("search_context_size"),
            Some(&serde_json::json!("high"))
        );
        assert_eq!(serde_json::to_value(&tool).unwrap(), wire);
    }

    #[test]
    fn web_search_tool_allowed_domains_is_typed() {
        let wire = serde_json::json!({
            "type": "cf_gears:web_search",
            "allowed_domains": ["docs.rs", "serde.rs"],
            "search_context_size": "low"
        });
        let tool: Tool = serde_json::from_value(wire.clone()).unwrap();
        let Tool::WebSearch(web_search) = &tool else {
            panic!("expected WebSearch, got {tool:?}");
        };
        assert_eq!(
            web_search.allowed_domains.as_deref(),
            Some(&["docs.rs".to_owned(), "serde.rs".to_owned()][..])
        );
        assert!(!web_search.options.contains_key("allowed_domains"));
        assert_eq!(
            web_search.options.get("search_context_size"),
            Some(&serde_json::json!("low"))
        );
        assert_eq!(serde_json::to_value(&tool).unwrap(), wire);
    }

    #[test]
    fn provider_tool_is_preserved_verbatim() {
        let wire = serde_json::json!({ "type": "openai:shell", "env": "default" });
        let tool: Tool = serde_json::from_value(wire.clone()).unwrap();
        let Tool::Other(ext) = &tool else {
            panic!("expected Other");
        };
        assert_eq!(ext.kind(), Some("openai:shell"));
        assert_eq!(serde_json::to_value(&tool).unwrap(), wire);
    }
}
