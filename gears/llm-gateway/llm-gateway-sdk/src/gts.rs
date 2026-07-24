//! GTS schema definition for LLM Gateway provider plugins.
//!
//! The LLM Gateway (main gear) registers this schema in the types-registry
//! during `init()`. Each provider plugin registers an instance of it (plus a
//! scoped `ClientHub` client) so the gateway can discover and resolve the
//! plugin at runtime.
//!
//! Selection keys on **provider identity**: the instance's `provider_type`
//! carries the Model Registry model-info `gts_type` the plugin serves
//! (e.g. `gts.cf.genai.model.info.v1~cf.genai._.openai.v1~`). The gateway
//! resolves the model, reads its `gts_type`, and picks the plugin whose
//! `provider_type` matches (ties broken on `PluginV1.priority`).

use gts::GtsTypeId;
use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

/// GTS type definition for LLM Gateway provider plugin instances.
///
/// # Instance ID format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~cf.llmgw.provider.plugin.v1~<vendor>.<package>.<name>.plugin.v1
/// ```
///
/// # Example
///
/// ```ignore
/// let instance_id = LlmGatewayProviderPluginSpecV1::gts_make_instance_id(
///     "cf.builtin.openai.plugin.v1",
/// );
///
/// let instance = PluginV1::<LlmGatewayProviderPluginSpecV1> {
///     id: instance_id.clone(),
///     vendor: "constructorfabric".to_owned(),
///     priority: 100,
///     properties: LlmGatewayProviderPluginSpecV1 {
///         provider_type: GtsTypeId::new("gts.cf.genai.model.info.v1~cf.genai._.openai.v1~"),
///     },
/// };
///
/// registry.register(vec![serde_json::to_value(&instance)?]).await?;
/// ```
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.llmgw.provider.plugin.v1~"),
    description = "LLM Gateway provider plugin specification",
    properties = "provider_type",
)]
pub struct LlmGatewayProviderPluginSpecV1 {
    /// Model Registry provider identity this plugin serves — the model-info
    /// `gts_type` (e.g. `gts.cf.genai.model.info.v1~cf.genai._.openai.v1~`).
    /// Matched against `ModelInfoV1.gts_type` during plugin resolution.
    pub provider_type: GtsTypeId,
}
