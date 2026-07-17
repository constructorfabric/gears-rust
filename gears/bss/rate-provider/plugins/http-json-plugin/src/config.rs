//! http-json source-plugin configuration.

use secrecy::SecretString;
use serde::Deserialize;

/// Config for the generic http-json source plugin.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpJsonPluginConfig {
    /// Stable `provider_id` stamped on synced ledger rows.
    pub id: String,
    /// Plugin-selection vendor (must match the core gear's configured vendor).
    pub vendor: String,
    /// Fallback order: lower `priority` is tried first by the composite.
    pub priority: i16,
    /// Feed endpoint (MUST be https; required — no sensible default).
    pub base_url: String,
    /// Outbound per-attempt HTTP timeout in milliseconds.
    pub timeout_ms: u64,
    /// Optional API key (`${VAR}` / `CredStore` expansion happens upstream).
    /// `SecretString` redacts the value from `Debug`/any accidental log line;
    /// only `secrecy::ExposeSecret::expose_secret()` reveals it.
    pub api_key: Option<SecretString>,
    /// How `api_key` is presented.
    pub auth: AuthKind,
    /// Field mapping (required for this kind).
    pub mapping: Option<Mapping>,
}

impl Default for HttpJsonPluginConfig {
    fn default() -> Self {
        Self {
            id: "http-json".to_owned(),
            vendor: "cf.bss".to_owned(),
            priority: 200,
            base_url: String::new(),
            timeout_ms: 5000,
            api_key: None,
            auth: AuthKind::None,
            mapping: None,
        }
    }
}

/// How an api key is presented on the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    /// No auth header.
    #[default]
    None,
    /// `Authorization: Bearer <api_key>`.
    Bearer,
    /// A custom header carrying the key (header name fixed to `X-API-Key` in v1).
    HeaderKey,
}

/// Field mapping for the http-json source (simple dotted paths, design O-11 scope).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    /// Base currency: a literal (single-base feed, e.g. "USD") — v1 supports literal only.
    pub base: String,
    /// Dotted path to the object of quote -> entry (e.g. "rates").
    pub rates: String,
    /// Field name of the numeric rate within each entry (e.g. "value").
    pub rate: String,
    /// Dotted path to the publication timestamp (e.g. "date").
    pub as_of: String,
}
