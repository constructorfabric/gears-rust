use toolkit::gts::PluginV1;
use toolkit_gts::gts_id;
use toolkit_gts::gts_type_schema;

/// Resource type for the mini-chat chat surface.
pub const CHAT_RESOURCE_TYPE: &str = gts_id!("cf.core.ai_chat.chat.v1~cf.core.mini_chat.chat.v1~");

/// Resource type for the mini-chat model surface.
pub const MODEL_RESOURCE_TYPE: &str =
    gts_id!("cf.core.ai_chat.model.v1~cf.core.mini_chat.model.v1~");

/// Resource type for the mini-chat user-quota surface.
pub const USER_QUOTA_RESOURCE_TYPE: &str =
    gts_id!("cf.core.ai_chat.user_quota.v1~cf.core.mini_chat.user_quota.v1~");

/// GTS type definition for mini-chat policy plugin instances.
///
/// Each plugin registers an instance of this type with its vendor-specific
/// instance ID. The mini-chat gear discovers plugins by querying
/// types-registry for instances matching this schema.
///
/// # Instance ID Format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~<vendor>.<package>.mini_chat_model_policy.plugin.v1~
/// ```
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.mini_chat_model_policy.plugin.v1~"),
    description = "Mini-Chat Policy plugin specification",
    properties = "",
)]
pub struct MiniChatModelPolicyPluginSpecV1;

/// GTS type definition for mini-chat audit plugin instances.
///
/// # Instance ID Format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~<vendor>.<package>.mini_chat_audit.plugin.v1~
/// ```
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.mini_chat_audit.plugin.v1~"),
    description = "Mini-Chat Audit plugin specification",
    properties = "",
)]
pub struct MiniChatAuditPluginSpecV1;
