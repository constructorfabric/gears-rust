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

/// Message resource — scoped by `owner_tenant_id`, `owner_id`, and `id`.
/// Point-op calls supply `resource_id` (matching the `messages` entity's
/// `resource_col = "message_id"`); list calls omit it.
pub const MESSAGE: ResourceType = ResourceType::from_static(
    gts_id!("cf.core.chat_engine.message.v1~"),
    &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::OWNER_ID,
        pep_properties::RESOURCE_ID,
    ],
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
pub const SESSION_TYPE: ResourceType =
    ResourceType::from_static(gts_id!("cf.core.chat_engine.session_type.v1~"), &[]);

#[cfg(test)]
mod tests {
    use super::*;
    use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
    use authz_resolver_sdk::models::{EvaluationResponse, EvaluationResponseContext};
    use authz_resolver_sdk::pep::{ConstraintCompileError, compile_to_access_scope};
    use uuid::Uuid;

    fn resource_id_decision(resource_id: Uuid) -> EvaluationResponse {
        EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::RESOURCE_ID,
                        [resource_id],
                    ))],
                }],
                ..Default::default()
            },
        }
    }

    /// A `resource_id` constraint on a MESSAGE point-op decision must COMPILE
    /// into a usable scope — the `messages` entity is Scopable with
    /// `resource_col = "message_id"`, so resource-specific policies must be
    /// expressible. Guards the PEP-contract ↔ entity-annotation alignment.
    #[test]
    fn message_supports_resource_id_constraints() {
        let mid = Uuid::new_v4();
        let scope = compile_to_access_scope(
            &resource_id_decision(mid),
            false,
            MESSAGE.supported_properties(),
        )
        .expect("resource_id constraint must compile for MESSAGE");
        assert!(!scope.is_unconstrained());
        assert!(scope.contains_uuid(pep_properties::RESOURCE_ID, mid));
    }

    /// Control: with the pre-fix owner-only contract the same constraint fails
    /// closed (`AllConstraintsFailed`) — proving the advertised `RESOURCE_ID`
    /// property, not incidental behavior, is what unblocks the policy.
    #[test]
    fn resource_id_constraint_fails_closed_when_unadvertised() {
        let owner_only = &[pep_properties::OWNER_TENANT_ID, pep_properties::OWNER_ID];
        let err = compile_to_access_scope(&resource_id_decision(Uuid::new_v4()), false, owner_only)
            .expect_err("an unadvertised resource_id property must fail closed");
        assert!(matches!(
            err,
            ConstraintCompileError::AllConstraintsFailed { .. }
        ));
    }
}
