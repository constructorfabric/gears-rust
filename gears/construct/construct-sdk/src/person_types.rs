//! Construct's person types: one GTS graph node type per category of a person's profile.
//!
//! Each type derives from graph storage's owned node. Its `payload` holds exactly one property, so one node is one
//! value of one property. The schemas are embedded from the crate's `schemas/`, so the registration and the published
//! files cannot drift apart.

use toolkit_gts::gts_id;

/// A person's identity: names, external ids, public profiles, location, background.
pub const IDENTITY_TYPE: &str =
    gts_id!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.identity.v1~");
/// A person's roles: current role, education, programme, affiliations and work history.
pub const ROLES_TYPE: &str =
    gts_id!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.roles.v1~");
/// A person's skills: skills, languages, research areas, publications, awards and research metrics.
pub const SKILLS_TYPE: &str =
    gts_id!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.skills.v1~");
/// How a person wants to be served: language, format, tone, accessibility, constraints and goals.
pub const PREFERENCES_TYPE: &str = gts_id!(
    "cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.preferences.v1~"
);

/// The person types with their JSON schemas, in registration order.
pub const PERSON_TYPES: [(&str, &str); 4] = [
    (
        IDENTITY_TYPE,
        include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.identity.v1~.schema.json"
        ),
    ),
    (
        ROLES_TYPE,
        include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.roles.v1~.schema.json"
        ),
    ),
    (
        SKILLS_TYPE,
        include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.skills.v1~.schema.json"
        ),
    ),
    (
        PREFERENCES_TYPE,
        include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.preferences.v1~.schema.json"
        ),
    ),
];

#[cfg(test)]
#[path = "person_types_tests.rs"]
mod person_types_tests;
