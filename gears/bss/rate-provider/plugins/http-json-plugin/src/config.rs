//! http-json source-plugin configuration.

use secrecy::SecretString;
use serde::Deserialize;
use toolkit::var_expand::{ExpandVars, ExpandVarsError};

/// Config for the generic http-json source plugin.
#[derive(Debug, Clone, Deserialize, toolkit::ExpandVars)]
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
    /// How the feed is authenticated, and the credential it needs (if any).
    ///
    /// Marked for `${VAR}` expansion so the credential can be kept out of the
    /// config file itself — see [`Auth`]'s [`ExpandVars`] impl.
    #[expand_vars]
    pub auth: Auth,
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
            auth: Auth::None {},
            mapping: None,
        }
    }
}

/// How the feed is authenticated — the credential is carried by the variant that
/// needs it, so an unusable combination cannot be configured in the first place.
///
/// Both bad combinations — a kind that needs a key with no key set, and a key
/// attached to `none` — are `serde` errors at config load, not runtime checks:
/// invalid states are unrepresentable rather than validated.
///
/// YAML shape (omit `auth:` entirely for the no-credential case):
///
/// ```yaml
/// auth:
///   kind: bearer          # none | bearer | header-key
///   api_key: "${BANK_X_KEY}"
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Auth {
    /// No credential is sent (a public feed like ECB's).
    ///
    /// An empty struct variant rather than a unit one: `deny_unknown_fields` is not
    /// enforced for unit variants of an internally-tagged enum, so `kind: none`
    /// with a stray `api_key` beside it would parse and then silently send
    /// unauthenticated requests. As `None {}` the stray field is rejected.
    None {},
    /// `Authorization: Bearer <api_key>`.
    Bearer {
        /// `${VAR}` placeholders are expanded from the environment at `init()`
        /// (see [`Auth`]'s [`ExpandVars`] impl); `SecretString` keeps the value
        /// out of `Debug` and any accidental log line.
        api_key: SecretString,
    },
    /// A custom header carrying the key (header name fixed to `X-API-Key` in v1).
    HeaderKey {
        /// See [`Auth::Bearer::api_key`].
        api_key: SecretString,
    },
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

/// Expands `${VAR}` placeholders in the credential the variant carries.
///
/// Hand-written because `#[derive(ExpandVars)]` only supports structs with named
/// fields, while the credential deliberately lives inside an enum variant. Without
/// this, `api_key: "${BANK_X_KEY}"` would be sent to the feed as that literal
/// string, leaving the raw key in the config file as the only working option —
/// and `toolkit`'s effective-config dump serializes the config file *before*
/// typed deserialization, so a hardcoded key is disclosed verbatim by a routine
/// diagnostic while a placeholder is not (DESIGN §2.2 "Secrets handling").
///
/// A missing variable is an error, never a silent fallback: `init()` fails loudly
/// rather than the source authenticating with a placeholder.
impl ExpandVars for Auth {
    fn expand_vars(&mut self) -> Result<(), ExpandVarsError> {
        match self {
            Self::None {} => Ok(()),
            Self::Bearer { api_key } | Self::HeaderKey { api_key } => api_key.expand_vars(),
        }
    }
}

impl Default for Auth {
    /// A public, unauthenticated feed — hand-written because `#[derive(Default)]`
    /// only accepts a unit variant as the default, and the no-credential variant
    /// is deliberately an empty struct variant (see its docs).
    fn default() -> Self {
        Self::None {}
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
