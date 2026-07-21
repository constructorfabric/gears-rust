//! Canonical PEP `ResourceType` descriptors for Chat Engine.
//!
//! Exactly four resource types cover all PDP decisions in this gear
//! (DESIGN §3.5.3). Secondary services (variant, search, export, intelligence)
//! reuse these constants; they do NOT define additional resource types.
//
// @cpt-cf-chat-engine-interface-pep

use authz_resolver_sdk::pep::ResourceType;
use toolkit_gts::gts_id;
use toolkit_security::pep_properties;

/// Session resource — scoped by `owner_tenant_id`, `owner_id`, and `id`.
pub const SESSION: ResourceType = ResourceType::from_static(
    gts_id!("cf.core.chat_engine.session.v1~"),
    &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::OWNER_ID,
        pep_properties::RESOURCE_ID,
    ],
);

/// Message resource — scoped by `owner_tenant_id` and `owner_id`.
/// Point-op calls supply `resource_id`; list calls omit it.
pub const MESSAGE: ResourceType = ResourceType::from_static(
    gts_id!("cf.core.chat_engine.message.v1~"),
    &[pep_properties::OWNER_TENANT_ID, pep_properties::OWNER_ID],
);

/// Reaction resource — scoped by `owner_tenant_id` and `owner_id`.
pub const REACTION: ResourceType = ResourceType::from_static(
    gts_id!("cf.core.chat_engine.reaction.v1~"),
    &[pep_properties::OWNER_TENANT_ID, pep_properties::OWNER_ID],
);

/// Session-type resource — decision-gate only; no SQL constraints.
///
/// `session_types` is a globally non-tenant table (DESIGN §3.5.1); the PDP
/// governs mutation permission (`create`/`update`/`delete`) via allow/deny
/// decision only. No `supported_properties` are advertised.
pub const SESSION_TYPE: ResourceType = ResourceType::from_static(
    gts_id!("cf.core.chat_engine.session_type.v1~"),
    &[],
);
