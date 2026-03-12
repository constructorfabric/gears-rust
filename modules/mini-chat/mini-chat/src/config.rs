use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::infra::llm::ProviderKind;
use crate::module::DEFAULT_URL_PREFIX;

#[derive(Debug, Clone, Serialize, Deserialize, modkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct MiniChatConfig {
    #[serde(default = "default_url_prefix")]
    pub url_prefix: String,
    #[serde(default)]
    pub streaming: StreamingConfig,
    #[serde(default = "default_vendor")]
    pub vendor: String,
    #[serde(default)]
    pub estimation_budgets: EstimationBudgets,
    #[serde(default)]
    pub quota: QuotaConfig,
    #[serde(default)]
    pub outbox: OutboxConfig,
    /// Provider registry. Key = `provider_id` (matches [`ModelCatalogEntry::provider_id`]).
    #[expand_vars]
    #[serde(default = "default_providers")]
    pub providers: HashMap<String, ProviderEntry>,

    /// OAGW upstream alias for AI provider APIs.
    #[serde(default = "default_oagw_alias")]
    pub oagw_alias: String,

    /// Storage backend identifier (written to `attachments.storage_backend`).
    #[serde(default = "default_storage_backend")]
    pub storage_backend: String,

    /// Uploads / Attachments / Images configuration (DESIGN B.8 + B.7 RAG limits).
    #[serde(default)]
    pub attachments: AttachmentConfig,
}

/// Configuration for a single LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize, modkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    /// Which adapter to use (e.g., `openai_responses`, `openai_chat_completions`).
    pub kind: ProviderKind,
    /// OAGW upstream alias (used in proxy URI: `/{alias}/...`).
    /// Defaults to [`host`](ProviderEntry::host) when omitted.
    #[serde(default)]
    pub upstream_alias: Option<String>,
    /// Upstream hostname (e.g., `api.openai.com`). Used for OAGW upstream
    /// registration during module init.
    pub host: String,
    /// API path template for the responses endpoint.
    /// Use `{model}` as placeholder for the deployment/model name.
    /// Defaults to `/v1/responses` (`OpenAI` native).
    /// Azure example: `/openai/deployments/{model}/responses?api-version=2025-03-01-preview`
    #[serde(default = "default_api_path")]
    pub api_path: String,
    /// OAGW auth plugin type for this upstream (optional).
    /// Example: `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.apikey.v1`
    #[serde(default)]
    pub auth_plugin_type: Option<String>,
    /// Auth plugin config (e.g., `header`, `prefix`, `secret_ref`).
    /// Values support `${VAR}` env expansion via [`config_expanded()`].
    #[expand_vars]
    #[serde(default)]
    pub auth_config: Option<HashMap<String, String>>,
}

impl ProviderEntry {
    /// Effective OAGW upstream alias — falls back to [`host`](Self::host) when
    /// [`upstream_alias`](Self::upstream_alias) is not explicitly configured.
    #[must_use]
    pub fn effective_alias(&self) -> &str {
        self.upstream_alias.as_deref().unwrap_or(&self.host)
    }

    /// Validate provider entry at startup.
    pub fn validate(&self, provider_id: &str) -> Result<(), String> {
        if self.host.trim().is_empty() {
            return Err(format!("provider '{provider_id}': host must not be empty"));
        }
        Ok(())
    }
}

fn default_api_path() -> String {
    "/v1/responses".to_owned()
}

fn default_providers() -> HashMap<String, ProviderEntry> {
    let mut m = HashMap::new();
    m.insert(
        "openai".to_owned(),
        ProviderEntry {
            kind: ProviderKind::OpenAiResponses,
            upstream_alias: None,
            host: "api.openai.com".to_owned(),
            api_path: default_api_path(),
            auth_plugin_type: Some(
                "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.apikey.v1".to_owned(),
            ),
            auth_config: Some({
                let mut c = HashMap::new();
                c.insert("header".to_owned(), "Authorization".to_owned());
                c.insert("prefix".to_owned(), "Bearer ".to_owned());
                c.insert("secret_ref".to_owned(), "cred://openai-key".to_owned());
                c
            }),
        },
    );
    m
}

/// Uploads, attachments, images, thumbnails, and RAG-limit configuration.
///
/// Corresponds to DESIGN appendix sections B.7 (File search / RAG) and B.8
/// (Uploads / Attachments / Images).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentConfig {
    // --- Thumbnail settings (B.8) ---
    /// Target thumbnail width in pixels.
    #[serde(default = "default_thumbnail_width")]
    pub thumbnail_width: u32,

    /// Target thumbnail height in pixels.
    #[serde(default = "default_thumbnail_height")]
    pub thumbnail_height: u32,

    /// Max decoded thumbnail size in bytes (default: 128 KiB).
    #[serde(default = "default_thumbnail_max_bytes")]
    pub thumbnail_max_bytes: usize,

    /// Max source image pixels before skipping generation (pre-screen heuristic).
    #[serde(default = "default_thumbnail_max_pixels")]
    pub thumbnail_max_pixels: u64,

    /// SECURITY BOUNDARY: max bytes the image decoder may consume.
    /// Protects against pixel-bomb attacks (default: 32 MiB).
    #[serde(default = "default_thumbnail_max_decode_bytes")]
    pub thumbnail_max_decode_bytes: usize,

    // --- Doc summary settings ---
    /// Enable/disable async doc summary generation on document upload (default: false).
    /// When disabled, documents are still indexed in the vector store for `file_search`.
    #[serde(default)]
    pub doc_summary_enabled: bool,

    /// Model to use for document summarization.
    #[serde(default = "default_doc_summary_model")]
    pub doc_summary_model: String,

    /// System prompt for document summarization.
    #[serde(default = "default_doc_summary_prompt")]
    pub doc_summary_prompt: String,

    /// Provider ID for document summarization (must match a key in `providers`).
    #[serde(default = "default_doc_summary_provider_id")]
    pub doc_summary_provider_id: String,

    /// Maximum character count of document text sent to the LLM for summarization.
    /// Documents exceeding this limit are truncated (with a trailing note).
    #[serde(default = "default_doc_summary_max_input_chars")]
    pub doc_summary_max_input_chars: usize,

    // --- Upload limits (B.8) ---
    /// Per-file size limit in bytes (default: 25 MiB).
    #[serde(default = "default_max_upload_size_bytes")]
    pub max_upload_size_bytes: usize,

    // --- Daily per-user quota (DESIGN §Error Catalogue: quota_exceeded / uploads) ---
    /// Maximum uploads per user per day across all chats (default: 200).
    /// Set to 0 to disable the daily quota check.
    #[serde(default = "default_max_uploads_per_user_per_day")]
    pub max_uploads_per_user_per_day: usize,

    // --- RAG Quality & Scale Controls (B.7) ---
    /// Maximum document attachments per chat (default: 50).
    #[serde(default = "default_max_documents_per_chat")]
    pub max_documents_per_chat: usize,

    /// Max total document MB per chat (default: 100).
    #[serde(default = "default_max_total_upload_mb_per_chat")]
    pub max_total_upload_mb_per_chat: usize,

    // --- Worker settings ---
    /// Bounded mpsc channel capacity for the attachment worker (default: 16).
    ///
    /// **Memory implication**: each queued command holds the full file `Bytes` in
    /// memory. With `max_upload_size_bytes = 25 MiB`, worst-case memory for a full
    /// channel is `channel_size × 25 MiB`. Keep this value small (8–16) to bound
    /// backpressure memory.
    #[serde(default = "default_worker_channel_size")]
    pub worker_channel_size: usize,
}

/// SSE streaming tuning parameters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamingConfig {
    /// Bounded mpsc channel capacity between provider task and SSE writer.
    /// Valid range: 16–64 (default 32).
    #[serde(default = "default_channel_capacity")]
    pub sse_channel_capacity: u16,

    /// Ping keepalive interval in seconds.
    /// Valid range: 5–60 (default 15).
    #[serde(default = "default_ping_interval")]
    pub sse_ping_interval_seconds: u16,

    /// Maximum output tokens sent to the preflight reserve.
    /// Default 32768 (matching common model limits).
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            sse_channel_capacity: default_channel_capacity(),
            sse_ping_interval_seconds: default_ping_interval(),
            max_output_tokens: default_max_output_tokens(),
        }
    }
}

fn default_max_output_tokens() -> u32 {
    32_768
}

impl StreamingConfig {
    /// Validate configuration values at startup. Returns an error message
    /// describing the first invalid value found.
    pub fn validate(self) -> Result<(), String> {
        if !(16..=64).contains(&self.sse_channel_capacity) {
            return Err(format!(
                "sse_channel_capacity must be 16-64, got {}",
                self.sse_channel_capacity
            ));
        }
        if !(5..=60).contains(&self.sse_ping_interval_seconds) {
            return Err(format!(
                "sse_ping_interval_seconds must be 5-60, got {}",
                self.sse_ping_interval_seconds
            ));
        }
        Ok(())
    }
}

fn default_channel_capacity() -> u16 {
    32
}

fn default_ping_interval() -> u16 {
    15
}

impl Default for MiniChatConfig {
    fn default() -> Self {
        Self {
            url_prefix: default_url_prefix(),
            streaming: StreamingConfig::default(),
            vendor: default_vendor(),
            estimation_budgets: EstimationBudgets::default(),
            quota: QuotaConfig::default(),
            outbox: OutboxConfig::default(),
            providers: default_providers(),
            oagw_alias: default_oagw_alias(),
            storage_backend: default_storage_backend(),
            attachments: AttachmentConfig::default(),
        }
    }
}

impl AttachmentConfig {
    /// Validate attachment configuration at startup.
    pub fn validate(&self) -> Result<(), String> {
        if self.thumbnail_width == 0 {
            return Err("thumbnail_width must be > 0".to_owned());
        }
        if self.thumbnail_height == 0 {
            return Err("thumbnail_height must be > 0".to_owned());
        }
        if self.thumbnail_max_bytes == 0 {
            return Err("thumbnail_max_bytes must be > 0".to_owned());
        }
        if self.thumbnail_max_pixels == 0 {
            return Err("thumbnail_max_pixels must be > 0".to_owned());
        }
        if self.thumbnail_max_decode_bytes == 0 {
            return Err("thumbnail_max_decode_bytes must be > 0".to_owned());
        }
        if self.doc_summary_model.trim().is_empty() {
            return Err("doc_summary_model must be non-empty".to_owned());
        }
        if self.max_upload_size_bytes == 0 {
            return Err("max_upload_size_bytes must be > 0".to_owned());
        }
        if self.max_documents_per_chat == 0 {
            return Err("max_documents_per_chat must be > 0".to_owned());
        }
        if self.max_total_upload_mb_per_chat == 0 {
            return Err("max_total_upload_mb_per_chat must be > 0".to_owned());
        }
        Ok(())
    }
}

impl Default for AttachmentConfig {
    fn default() -> Self {
        Self {
            thumbnail_width: default_thumbnail_width(),
            thumbnail_height: default_thumbnail_height(),
            thumbnail_max_bytes: default_thumbnail_max_bytes(),
            thumbnail_max_pixels: default_thumbnail_max_pixels(),
            thumbnail_max_decode_bytes: default_thumbnail_max_decode_bytes(),
            doc_summary_enabled: false,
            doc_summary_model: default_doc_summary_model(),
            doc_summary_prompt: default_doc_summary_prompt(),
            doc_summary_provider_id: default_doc_summary_provider_id(),
            doc_summary_max_input_chars: default_doc_summary_max_input_chars(),
            max_upload_size_bytes: default_max_upload_size_bytes(),
            max_uploads_per_user_per_day: default_max_uploads_per_user_per_day(),
            max_documents_per_chat: default_max_documents_per_chat(),
            max_total_upload_mb_per_chat: default_max_total_upload_mb_per_chat(),
            worker_channel_size: default_worker_channel_size(),
        }
    }
}

/// Token estimation parameters sourced from `ConfigMap` (P1).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EstimationBudgets {
    #[serde(default = "default_bytes_per_token")]
    pub bytes_per_token_conservative: u32,
    #[serde(default = "default_fixed_overhead")]
    pub fixed_overhead_tokens: u32,
    #[serde(default = "default_safety_margin")]
    pub safety_margin_pct: u32,
    #[serde(default = "default_image_budget")]
    pub image_token_budget: u32,
    #[serde(default = "default_tool_surcharge")]
    pub tool_surcharge_tokens: u32,
    #[serde(default = "default_web_surcharge")]
    pub web_search_surcharge_tokens: u32,
    #[serde(default = "default_min_gen_floor")]
    pub minimal_generation_floor: u32,
}

impl Default for EstimationBudgets {
    fn default() -> Self {
        Self {
            bytes_per_token_conservative: default_bytes_per_token(),
            fixed_overhead_tokens: default_fixed_overhead(),
            safety_margin_pct: default_safety_margin(),
            image_token_budget: default_image_budget(),
            tool_surcharge_tokens: default_tool_surcharge(),
            web_search_surcharge_tokens: default_web_surcharge(),
            minimal_generation_floor: default_min_gen_floor(),
        }
    }
}

impl EstimationBudgets {
    pub fn validate(self) -> Result<(), String> {
        if self.bytes_per_token_conservative == 0 {
            return Err("bytes_per_token_conservative must be > 0".to_owned());
        }
        if self.minimal_generation_floor == 0 {
            return Err("minimal_generation_floor must be > 0".to_owned());
        }
        Ok(())
    }
}

fn default_bytes_per_token() -> u32 {
    4
}
fn default_fixed_overhead() -> u32 {
    100
}
fn default_safety_margin() -> u32 {
    10
}
fn default_image_budget() -> u32 {
    1000
}
fn default_tool_surcharge() -> u32 {
    500
}
fn default_web_surcharge() -> u32 {
    500
}
fn default_min_gen_floor() -> u32 {
    50
}

/// Quota enforcement configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaConfig {
    #[serde(default = "default_overshoot_tolerance")]
    pub overshoot_tolerance_factor: f64,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            overshoot_tolerance_factor: default_overshoot_tolerance(),
        }
    }
}

impl QuotaConfig {
    pub fn validate(self) -> Result<(), String> {
        if !(1.0..=1.5).contains(&self.overshoot_tolerance_factor) {
            return Err(format!(
                "overshoot_tolerance_factor must be 1.0-1.5, got {}",
                self.overshoot_tolerance_factor
            ));
        }
        Ok(())
    }
}

/// Outbox configuration for usage event publishing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxConfig {
    /// Queue name for usage events.
    #[serde(default = "default_outbox_queue_name")]
    pub queue_name: String,

    /// Number of outbox partitions. Must be 1–64.
    #[serde(default = "default_outbox_num_partitions")]
    pub num_partitions: u32,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            queue_name: default_outbox_queue_name(),
            num_partitions: default_outbox_num_partitions(),
        }
    }
}

impl OutboxConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.queue_name.trim().is_empty() {
            return Err("outbox queue_name must not be empty".to_owned());
        }
        if !(1..=64).contains(&self.num_partitions) || !self.num_partitions.is_power_of_two() {
            return Err(format!(
                "outbox num_partitions must be a power of 2 in 1-64, got {}",
                self.num_partitions
            ));
        }
        Ok(())
    }
}

fn default_outbox_queue_name() -> String {
    "mini-chat.usage_snapshot".to_owned()
}

fn default_outbox_num_partitions() -> u32 {
    4
}

fn default_overshoot_tolerance() -> f64 {
    1.10
}

fn default_url_prefix() -> String {
    DEFAULT_URL_PREFIX.to_owned()
}

fn default_vendor() -> String {
    "hyperspot".to_owned()
}

fn default_oagw_alias() -> String {
    "openai".to_owned()
}

fn default_storage_backend() -> String {
    "azure".to_owned()
}

fn default_thumbnail_width() -> u32 {
    128
}

fn default_thumbnail_height() -> u32 {
    128
}

fn default_thumbnail_max_bytes() -> usize {
    131_072 // 128 KiB
}

fn default_thumbnail_max_pixels() -> u64 {
    100_000_000
}

fn default_thumbnail_max_decode_bytes() -> usize {
    33_554_432 // 32 MiB
}

fn default_doc_summary_model() -> String {
    "gpt-4o-mini".to_owned()
}

fn default_doc_summary_prompt() -> String {
    "Summarize the following document concisely. Focus on key topics, purpose, and main conclusions.".to_owned()
}

fn default_doc_summary_provider_id() -> String {
    "openai".to_owned()
}

fn default_doc_summary_max_input_chars() -> usize {
    100_000
}

fn default_max_upload_size_bytes() -> usize {
    26_214_400 // 25 MiB
}

fn default_max_uploads_per_user_per_day() -> usize {
    200
}

fn default_max_documents_per_chat() -> usize {
    50
}

fn default_max_total_upload_mb_per_chat() -> usize {
    100
}

fn default_worker_channel_size() -> usize {
    16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        StreamingConfig::default().validate().unwrap();
        EstimationBudgets::default().validate().unwrap();
        QuotaConfig::default().validate().unwrap();
        OutboxConfig::default().validate().unwrap();
        AttachmentConfig::default().validate().unwrap();
    }

    #[test]
    fn default_config_matches_design_spec() {
        let config = MiniChatConfig::default();
        assert_eq!(config.url_prefix, "/mini-chat");
        assert_eq!(config.oagw_alias, "openai");
        assert_eq!(config.storage_backend, "azure");
        let att = &config.attachments;
        assert_eq!(att.thumbnail_width, 128);
        assert_eq!(att.thumbnail_height, 128);
        assert_eq!(att.thumbnail_max_bytes, 131_072);
        assert_eq!(att.thumbnail_max_pixels, 100_000_000);
        assert_eq!(att.thumbnail_max_decode_bytes, 33_554_432);
        assert_eq!(att.max_upload_size_bytes, 26_214_400);
        assert_eq!(att.max_documents_per_chat, 50);
        assert_eq!(att.max_total_upload_mb_per_chat, 100);
        assert_eq!(att.doc_summary_model, "gpt-4o-mini");
    }

    #[test]
    fn estimation_budgets_validation() {
        let valid = EstimationBudgets::default();

        assert!(
            (EstimationBudgets {
                bytes_per_token_conservative: 0,
                ..valid
            })
            .validate()
            .is_err()
        );
        assert!(
            (EstimationBudgets {
                minimal_generation_floor: 0,
                ..valid
            })
            .validate()
            .is_err()
        );
    }

    #[test]
    fn quota_config_validation() {
        assert!(
            (QuotaConfig {
                overshoot_tolerance_factor: 0.99
            })
            .validate()
            .is_err()
        );
        assert!(
            (QuotaConfig {
                overshoot_tolerance_factor: 1.0
            })
            .validate()
            .is_ok()
        );
        assert!(
            (QuotaConfig {
                overshoot_tolerance_factor: 1.5
            })
            .validate()
            .is_ok()
        );
        assert!(
            (QuotaConfig {
                overshoot_tolerance_factor: 1.51
            })
            .validate()
            .is_err()
        );
    }

    #[test]
    fn channel_capacity_boundaries() {
        let valid = StreamingConfig::default();

        assert!(
            (StreamingConfig {
                sse_channel_capacity: 15,
                ..valid
            })
            .validate()
            .is_err()
        );
        assert!(
            (StreamingConfig {
                sse_channel_capacity: 16,
                ..valid
            })
            .validate()
            .is_ok()
        );
        assert!(
            (StreamingConfig {
                sse_channel_capacity: 64,
                ..valid
            })
            .validate()
            .is_ok()
        );
        assert!(
            (StreamingConfig {
                sse_channel_capacity: 65,
                ..valid
            })
            .validate()
            .is_err()
        );
    }

    #[test]
    fn ping_interval_boundaries() {
        let valid = StreamingConfig::default();

        assert!(
            (StreamingConfig {
                sse_ping_interval_seconds: 4,
                ..valid
            })
            .validate()
            .is_err()
        );
        assert!(
            (StreamingConfig {
                sse_ping_interval_seconds: 5,
                ..valid
            })
            .validate()
            .is_ok()
        );
        assert!(
            (StreamingConfig {
                sse_ping_interval_seconds: 60,
                ..valid
            })
            .validate()
            .is_ok()
        );
        assert!(
            (StreamingConfig {
                sse_ping_interval_seconds: 61,
                ..valid
            })
            .validate()
            .is_err()
        );
    }

    #[test]
    fn provider_entry_deser_with_alias() {
        let json = r#"{
            "kind": "openai_responses",
            "host": "api.openai.com",
            "upstream_alias": "custom-alias"
        }"#;
        let entry: ProviderEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.host, "api.openai.com");
        assert_eq!(entry.effective_alias(), "custom-alias");
        assert!(entry.auth_plugin_type.is_none());
    }

    #[test]
    fn provider_entry_deser_without_alias() {
        let json = r#"{
            "kind": "openai_responses",
            "host": "my-azure.openai.azure.com",
            "api_path": "/openai/v1/responses"
        }"#;
        let entry: ProviderEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.effective_alias(), "my-azure.openai.azure.com");
        assert_eq!(entry.api_path, "/openai/v1/responses");
    }

    #[test]
    fn provider_entry_deser_with_auth() {
        let json = r#"{
            "kind": "openai_responses",
            "host": "api.openai.com",
            "auth_plugin_type": "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.apikey.v1",
            "auth_config": {
                "header": "Authorization",
                "prefix": "Bearer ",
                "secret_ref": "cred://openai-key"
            }
        }"#;
        let entry: ProviderEntry = serde_json::from_str(json).unwrap();
        assert!(entry.auth_plugin_type.is_some());
        let config = entry.auth_config.unwrap();
        assert_eq!(config.get("header").unwrap(), "Authorization");
        assert_eq!(config.get("secret_ref").unwrap(), "cred://openai-key");
    }

    #[test]
    fn default_providers_has_openai() {
        let cfg = MiniChatConfig::default();
        assert!(cfg.providers.contains_key("openai"));
        let openai = &cfg.providers["openai"];
        assert_eq!(openai.effective_alias(), "api.openai.com");
        assert_eq!(openai.api_path, "/v1/responses");
    }
}
