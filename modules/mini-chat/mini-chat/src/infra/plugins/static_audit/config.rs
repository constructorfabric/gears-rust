use serde::Deserialize;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticMiniChatAuditPluginConfig {
    /// When `false`, the plugin registers but does not emit audit events.
    /// Defaults to `true`.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl Default for StaticMiniChatAuditPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
        }
    }
}

const fn default_enabled() -> bool {
    true
}
