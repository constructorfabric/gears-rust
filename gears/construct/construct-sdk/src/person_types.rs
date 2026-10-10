//! Construct's person types: one GTS graph node type per category of a person's profile.
//!
//! Each type derives from graph storage's owned node. One node is one fact: its `payload` holds one property, which
//! names the fact, and the fact's origin. The schemas are embedded from the crate's `schemas/`, so the registration and
//! the published files cannot drift apart.

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
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-person-types:p1
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

/// How many values of one property a subject may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cardinality {
    /// At most one value. A new value replaces the stored one.
    One,
    /// Any number of values.
    Many,
}

/// The payload member that holds the origin of a fact: graph storage's provenance attribute.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-fact-origin:p1
pub const ORIGIN_PROPERTY: &str = "origin";

/// Every fact property of every person type, with its cardinality.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-cardinality:p1
pub const PROPERTIES: &[(&str, &str, Cardinality)] = &[
    (IDENTITY_TYPE, "background", Cardinality::One),
    (IDENTITY_TYPE, "full_name", Cardinality::One),
    (IDENTITY_TYPE, "google_scholar_id", Cardinality::One),
    (IDENTITY_TYPE, "headline", Cardinality::One),
    (IDENTITY_TYPE, "location", Cardinality::One),
    (IDENTITY_TYPE, "name_variants", Cardinality::Many),
    (IDENTITY_TYPE, "openalex_author_id", Cardinality::One),
    (IDENTITY_TYPE, "orcid_id", Cardinality::One),
    (IDENTITY_TYPE, "public_mentions", Cardinality::Many),
    (IDENTITY_TYPE, "public_profiles", Cardinality::Many),
    (IDENTITY_TYPE, "scopus_author_id", Cardinality::One),
    (IDENTITY_TYPE, "timezone", Cardinality::One),
    (PREFERENCES_TYPE, "analogy_domain", Cardinality::One),
    (PREFERENCES_TYPE, "answer_language", Cardinality::One),
    (PREFERENCES_TYPE, "assistive_tools", Cardinality::One),
    (PREFERENCES_TYPE, "average_session_length", Cardinality::One),
    (PREFERENCES_TYPE, "captions", Cardinality::One),
    (
        PREFERENCES_TYPE,
        "challenge_starting_level",
        Cardinality::One,
    ),
    (PREFERENCES_TYPE, "chunk_size", Cardinality::One),
    (PREFERENCES_TYPE, "citation_style", Cardinality::One),
    (PREFERENCES_TYPE, "cognitive_load_hint", Cardinality::One),
    (PREFERENCES_TYPE, "collaboration_style", Cardinality::One),
    (PREFERENCES_TYPE, "communication_channel", Cardinality::One),
    (PREFERENCES_TYPE, "content_maturity_level", Cardinality::One),
    (PREFERENCES_TYPE, "current_focus_areas", Cardinality::Many),
    (PREFERENCES_TYPE, "dialect", Cardinality::One),
    (PREFERENCES_TYPE, "dyslexia_friendly", Cardinality::One),
    (PREFERENCES_TYPE, "explainability", Cardinality::One),
    (PREFERENCES_TYPE, "feedback_style", Cardinality::One),
    (PREFERENCES_TYPE, "focus_windows", Cardinality::Many),
    (PREFERENCES_TYPE, "group_vs_individual", Cardinality::One),
    (PREFERENCES_TYPE, "high_contrast", Cardinality::One),
    (PREFERENCES_TYPE, "hint_depth", Cardinality::One),
    (PREFERENCES_TYPE, "interface_layout", Cardinality::One),
    (PREFERENCES_TYPE, "jargon_tolerance", Cardinality::One),
    (
        PREFERENCES_TYPE,
        "local_environment_constraint",
        Cardinality::One,
    ),
    (PREFERENCES_TYPE, "long_term_goals", Cardinality::Many),
    (PREFERENCES_TYPE, "math_notation", Cardinality::One),
    (PREFERENCES_TYPE, "modality_sensitivity", Cardinality::One),
    (PREFERENCES_TYPE, "motivation", Cardinality::One),
    (PREFERENCES_TYPE, "notification_type", Cardinality::One),
    (PREFERENCES_TYPE, "nudge_preference", Cardinality::One),
    (PREFERENCES_TYPE, "preferred_format", Cardinality::One),
    (PREFERENCES_TYPE, "presentation_order", Cardinality::One),
    (
        PREFERENCES_TYPE,
        "programming_language_target",
        Cardinality::One,
    ),
    (PREFERENCES_TYPE, "readiness_hint", Cardinality::One),
    (PREFERENCES_TYPE, "reading_level", Cardinality::One),
    (PREFERENCES_TYPE, "reduced_motion", Cardinality::One),
    (PREFERENCES_TYPE, "runtime_constraint", Cardinality::One),
    (PREFERENCES_TYPE, "self_efficacy", Cardinality::One),
    (PREFERENCES_TYPE, "short_term_goals", Cardinality::Many),
    (PREFERENCES_TYPE, "study_window", Cardinality::Many),
    (PREFERENCES_TYPE, "technology_constraint", Cardinality::One),
    (
        PREFERENCES_TYPE,
        "time_availability_pattern",
        Cardinality::Many,
    ),
    (PREFERENCES_TYPE, "time_constraint", Cardinality::One),
    (PREFERENCES_TYPE, "tone_preference", Cardinality::One),
    (PREFERENCES_TYPE, "tool_preference", Cardinality::One),
    (PREFERENCES_TYPE, "ui_language", Cardinality::One),
    (PREFERENCES_TYPE, "units", Cardinality::One),
    (ROLES_TYPE, "active_course", Cardinality::One),
    (ROLES_TYPE, "affiliation_history", Cardinality::Many),
    (ROLES_TYPE, "board_memberships", Cardinality::Many),
    (ROLES_TYPE, "current_affiliation", Cardinality::One),
    (ROLES_TYPE, "education_level", Cardinality::One),
    (ROLES_TYPE, "group_memberships", Cardinality::Many),
    (ROLES_TYPE, "instructor", Cardinality::One),
    (ROLES_TYPE, "major", Cardinality::One),
    (ROLES_TYPE, "program", Cardinality::One),
    (ROLES_TYPE, "role", Cardinality::One),
    (ROLES_TYPE, "roles_and_affiliations", Cardinality::Many),
    (ROLES_TYPE, "school_grade", Cardinality::One),
    (ROLES_TYPE, "section", Cardinality::One),
    (ROLES_TYPE, "work_history", Cardinality::Many),
    (SKILLS_TYPE, "activity_by_year", Cardinality::Many),
    (SKILLS_TYPE, "awards", Cardinality::Many),
    (SKILLS_TYPE, "career_stage", Cardinality::One),
    (SKILLS_TYPE, "first_publication_year", Cardinality::One),
    (SKILLS_TYPE, "h_index", Cardinality::One),
    (SKILLS_TYPE, "i10_index", Cardinality::One),
    (SKILLS_TYPE, "languages", Cardinality::Many),
    (SKILLS_TYPE, "latest_publication_year", Cardinality::One),
    (SKILLS_TYPE, "mean_citedness_2y", Cardinality::One),
    (SKILLS_TYPE, "primary_research_area", Cardinality::One),
    (SKILLS_TYPE, "publications", Cardinality::Many),
    (SKILLS_TYPE, "research_areas", Cardinality::Many),
    (SKILLS_TYPE, "skills", Cardinality::Many),
    (SKILLS_TYPE, "total_citations", Cardinality::One),
    (SKILLS_TYPE, "total_publications", Cardinality::One),
];

/// The cardinality of `property` in the person type `type_id`, or `None` when the type has no such property.
#[must_use]
pub fn cardinality(type_id: &str, property: &str) -> Option<Cardinality> {
    PROPERTIES
        .iter()
        .find(|(listed_type, listed_property, _)| {
            *listed_type == type_id && *listed_property == property
        })
        .map(|(_, _, cardinality)| *cardinality)
}

#[cfg(test)]
#[path = "person_types_tests.rs"]
mod person_types_tests;
