//! GTS identifiers for Chat Engine backend plugins.
//!
//! Every backend plugin instance (the built-in webhook-compat plugin, the
//! first-party LLM Gateway plugin, third-party plugins) is a GTS *instance*
//! of [`ChatEngineBackendPluginSpecV1`], which derives from the platform
//! base type `PluginV1`. The instance id is what session types persist in
//! `session_types.plugin_instance_id` / `plugin_configs.plugin_instance_id`
//! and what the `ClientHub` scope is keyed on.

use toolkit_gts::PluginV1;
use toolkit_gts::{gts_id, gts_type_schema};

/// GTS type definition for Chat Engine backend plugin instances.
///
/// # Instance ID format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~cf.chat_engine._.backend_plugin.v1~<vendor>.<package>.<name>.v1
/// ```
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.chat_engine._.backend_plugin.v1~"),
    description = "Chat Engine backend plugin specification",
    properties = "",
)]
pub struct ChatEngineBackendPluginSpecV1;

/// Instance id of the built-in webhook-compat plugin.
pub const WEBHOOK_COMPAT_PLUGIN_INSTANCE_ID: &str = gts_id!(
    "cf.toolkit.plugins.plugin.v1~cf.chat_engine._.backend_plugin.v1~cf.chat_engine._.webhook_compat.v1"
);

/// Instance id of the first-party LLM Gateway plugin.
pub const LLM_GATEWAY_PLUGIN_INSTANCE_ID: &str = gts_id!(
    "cf.toolkit.plugins.plugin.v1~cf.chat_engine._.backend_plugin.v1~cf.chat_engine._.llm_gateway.v1"
);

/// Pre-fix instance ids (invalid `gtx.` prefix, trailing `~`) that may still
/// be sent by API callers or sit in old config. Mapped to the current ids.
const LEGACY_PLUGIN_INSTANCE_IDS: [(&str, &str); 2] = [
    (
        "gtx.cf.chat_engine.webhook_compat_plugin.v1~",
        WEBHOOK_COMPAT_PLUGIN_INSTANCE_ID,
    ),
    (
        "gtx.cf.chat_engine.llm_gateway_plugin.v1~",
        LLM_GATEWAY_PLUGIN_INSTANCE_ID,
    ),
];

/// Maps a legacy plugin instance id to its current form; any other id is
/// returned unchanged.
pub fn canonical_plugin_instance_id(id: &str) -> &str {
    LEGACY_PLUGIN_INSTANCE_IDS
        .iter()
        .find(|(legacy, _)| *legacy == id)
        .map_or(id, |(_, current)| current)
}
