mod attachments;
mod chats;
mod messages;
mod models;
mod quota;
mod reactions;
mod turns;

use std::sync::Arc;

use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::LicenseFeature;

use crate::module::AppServices;

/// License feature required by all mini-chat endpoints.
///
/// DESIGN constraint `cpt-cf-mini-chat-constraint-license-gate`:
/// access requires the `gts.x.core.lic.feat.v1~x.core.global.base.v1` feature
/// on the tenant license.
pub(crate) struct AiChatLicense;

// TODO: Replace the base license feature name with the actual one
// once the license plugin can provide necessary information.
impl AsRef<str> for AiChatLicense {
    fn as_ref(&self) -> &'static str {
        "gts.x.core.lic.feat.v1~x.core.global.base.v1"
    }
}

impl LicenseFeature for AiChatLicense {}

/// Register all mini-chat REST routes.
pub(crate) fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    services: Arc<AppServices>,
    prefix: &str,
) -> Router {
    let router = chats::register_chat_routes(router, openapi, prefix);
    let router = messages::register_message_routes(router, openapi, prefix);
    let router = attachments::register_attachment_routes(router, openapi, prefix);
    let router = turns::register_turn_routes(router, openapi, prefix);
    let router = models::register_model_routes(router, openapi, prefix);
    let router = reactions::register_reaction_routes(router, openapi, prefix);
    let router = quota::register_quota_routes(router, openapi, prefix);

    router.layer(axum::Extension(services))
}
