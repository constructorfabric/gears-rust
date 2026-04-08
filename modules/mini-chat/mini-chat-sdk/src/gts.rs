use gts_macros::struct_to_gts_schema;
use modkit::gts::BaseModkitPluginV1;

/// GTS type definition for mini-chat policy plugin instances.
///
/// Each plugin registers an instance of this type with its vendor-specific
/// instance ID. The mini-chat module discovers plugins by querying
/// types-registry for instances matching this schema.
///
/// # Instance ID Format
///
/// ```text
/// gts.x.core.modkit.plugin.v1~<vendor>.<package>.mini_chat_model_policy.plugin.v1~
/// ```
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = BaseModkitPluginV1,
    schema_id = "gts.x.core.modkit.plugin.v1~x.core.mini_chat_model_policy.plugin.v1~",
    description = "Mini-Chat Policy plugin specification",
    properties = ""
)]
pub struct MiniChatModelPolicyPluginSpecV1;

/// GTS type definition for mini-chat audit plugin instances.
///
/// # Instance ID Format
///
/// ```text
/// gts.x.core.modkit.plugin.v1~<vendor>.<package>.mini_chat_audit.plugin.v1~
/// ```
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = BaseModkitPluginV1,
    schema_id = "gts.x.core.modkit.plugin.v1~x.core.mini_chat_audit.plugin.v1~",
    description = "Mini-Chat Audit plugin specification",
    properties = ""
)]
pub struct MiniChatAuditPluginSpecV1;
