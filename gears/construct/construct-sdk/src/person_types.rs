//! Construct's person types: one GTS graph node type per category of a person's profile.
//!
//! Each type derives from graph storage's owned node. One node is one fact. Its `payload` holds one property, which
//! names the fact, and `origin`, the origin of the fact. It also holds `record`, the record the fact came from, exactly
//! when the fact came from a record. The schemas are embedded from the crate's `schemas/`, so the registration and the
//! published files cannot drift apart.

use serde_json::{Map, Value};

use crate::gts::{IDENTITY_TYPE, PREFERENCE_TYPE, ROLE_TYPE, SKILL_TYPE};

/// One person type: its type id and its JSON schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonType {
    pub type_id: &'static str,
    pub schema: &'static str,
}

/// The person types with their JSON schemas, in registration order.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-person-types:p1
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-fact-origin:p1
pub const PERSON_TYPES: [PersonType; 4] = [
    PersonType {
        type_id: IDENTITY_TYPE,
        schema: include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.identity.v1~.schema.json"
        ),
    },
    PersonType {
        type_id: ROLE_TYPE,
        schema: include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.role.v1~.schema.json"
        ),
    },
    PersonType {
        type_id: SKILL_TYPE,
        schema: include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.skill.v1~.schema.json"
        ),
    },
    PersonType {
        type_id: PREFERENCE_TYPE,
        schema: include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.preference.v1~.schema.json"
        ),
    },
];

/// How many values of one property a subject may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cardinality {
    /// At most one value. A new value replaces the stored one.
    One,
    /// Any number of values.
    Many,
}

/// One fact property of a person type, with its cardinality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactProperty {
    pub type_id: &'static str,
    pub name: &'static str,
    pub cardinality: Cardinality,
}

const fn one(type_id: &'static str, name: &'static str) -> FactProperty {
    FactProperty {
        type_id,
        name,
        cardinality: Cardinality::One,
    }
}

const fn many(type_id: &'static str, name: &'static str) -> FactProperty {
    FactProperty {
        type_id,
        name,
        cardinality: Cardinality::Many,
    }
}

/// Every fact property of every person type, with its cardinality.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-cardinality:p1
pub const PROPERTIES: &[FactProperty] = &[
    one(IDENTITY_TYPE, "background"),
    one(IDENTITY_TYPE, "full_name"),
    one(IDENTITY_TYPE, "google_scholar_id"),
    one(IDENTITY_TYPE, "headline"),
    one(IDENTITY_TYPE, "location"),
    many(IDENTITY_TYPE, "name_variants"),
    one(IDENTITY_TYPE, "openalex_author_id"),
    one(IDENTITY_TYPE, "orcid_id"),
    many(IDENTITY_TYPE, "public_mentions"),
    many(IDENTITY_TYPE, "public_profiles"),
    one(IDENTITY_TYPE, "scopus_author_id"),
    one(IDENTITY_TYPE, "timezone"),
    one(PREFERENCE_TYPE, "analogy_domain"),
    one(PREFERENCE_TYPE, "answer_language"),
    many(PREFERENCE_TYPE, "assistive_tools"),
    one(PREFERENCE_TYPE, "average_session_length"),
    one(PREFERENCE_TYPE, "captions"),
    one(PREFERENCE_TYPE, "challenge_starting_level"),
    one(PREFERENCE_TYPE, "chunk_size"),
    one(PREFERENCE_TYPE, "citation_style"),
    one(PREFERENCE_TYPE, "cognitive_load_hint"),
    one(PREFERENCE_TYPE, "collaboration_style"),
    many(PREFERENCE_TYPE, "communication_channel"),
    one(PREFERENCE_TYPE, "content_maturity_level"),
    many(PREFERENCE_TYPE, "current_focus_areas"),
    one(PREFERENCE_TYPE, "dialect"),
    one(PREFERENCE_TYPE, "dyslexia_friendly"),
    one(PREFERENCE_TYPE, "explainability"),
    one(PREFERENCE_TYPE, "feedback_style"),
    many(PREFERENCE_TYPE, "focus_windows"),
    one(PREFERENCE_TYPE, "group_vs_individual"),
    one(PREFERENCE_TYPE, "high_contrast"),
    one(PREFERENCE_TYPE, "hint_depth"),
    one(PREFERENCE_TYPE, "interface_layout"),
    one(PREFERENCE_TYPE, "jargon_tolerance"),
    many(PREFERENCE_TYPE, "local_environment_constraint"),
    many(PREFERENCE_TYPE, "long_term_goals"),
    one(PREFERENCE_TYPE, "math_notation"),
    one(PREFERENCE_TYPE, "modality_sensitivity"),
    one(PREFERENCE_TYPE, "motivation"),
    many(PREFERENCE_TYPE, "notification_type"),
    one(PREFERENCE_TYPE, "nudge_preference"),
    many(PREFERENCE_TYPE, "preferred_format"),
    one(PREFERENCE_TYPE, "presentation_order"),
    one(PREFERENCE_TYPE, "programming_language_target"),
    one(PREFERENCE_TYPE, "readiness_hint"),
    one(PREFERENCE_TYPE, "reading_level"),
    one(PREFERENCE_TYPE, "reduced_motion"),
    one(PREFERENCE_TYPE, "runtime_constraint"),
    one(PREFERENCE_TYPE, "self_efficacy"),
    many(PREFERENCE_TYPE, "short_term_goals"),
    many(PREFERENCE_TYPE, "study_window"),
    many(PREFERENCE_TYPE, "technology_constraint"),
    many(PREFERENCE_TYPE, "time_availability_pattern"),
    many(PREFERENCE_TYPE, "time_constraint"),
    one(PREFERENCE_TYPE, "tone_preference"),
    one(PREFERENCE_TYPE, "tool_preference"),
    one(PREFERENCE_TYPE, "ui_language"),
    one(PREFERENCE_TYPE, "units"),
    one(ROLE_TYPE, "active_course"),
    many(ROLE_TYPE, "affiliation_history"),
    many(ROLE_TYPE, "board_memberships"),
    one(ROLE_TYPE, "current_affiliation"),
    one(ROLE_TYPE, "education_level"),
    many(ROLE_TYPE, "group_memberships"),
    one(ROLE_TYPE, "instructor"),
    one(ROLE_TYPE, "major"),
    one(ROLE_TYPE, "program"),
    one(ROLE_TYPE, "role"),
    many(ROLE_TYPE, "roles_and_affiliations"),
    one(ROLE_TYPE, "school_grade"),
    one(ROLE_TYPE, "section"),
    many(ROLE_TYPE, "work_history"),
    many(SKILL_TYPE, "activity_by_year"),
    many(SKILL_TYPE, "awards"),
    one(SKILL_TYPE, "career_stage"),
    one(SKILL_TYPE, "first_publication_year"),
    one(SKILL_TYPE, "h_index"),
    one(SKILL_TYPE, "i10_index"),
    many(SKILL_TYPE, "languages"),
    one(SKILL_TYPE, "latest_publication_year"),
    one(SKILL_TYPE, "mean_citedness_2y"),
    one(SKILL_TYPE, "primary_research_area"),
    many(SKILL_TYPE, "publications"),
    many(SKILL_TYPE, "research_areas"),
    many(SKILL_TYPE, "skills"),
    one(SKILL_TYPE, "total_citations"),
    one(SKILL_TYPE, "total_publications"),
];

/// The schema of `property` in a person type's parsed `schema`, with each reference into the type's own document
/// replaced by what it points to, so the schema stands alone. `None` when the type has no such property.
#[must_use]
pub fn property_schema(schema: &Value, property: &str) -> Option<Value> {
    schema
        .pointer("/allOf/1/properties/payload/properties")?
        .get(property)
        .map(|found| resolved(found, schema))
}

fn resolved(value: &Value, document: &Value) -> Value {
    match value {
        Value::Object(members) => {
            if let Some(Value::String(reference)) = members.get("$ref")
                && let Some(target) = reference
                    .strip_prefix('#')
                    .and_then(|pointer| document.pointer(pointer))
            {
                return resolved(target, document);
            }
            Value::Object(
                members
                    .iter()
                    .map(|(name, member)| (name.clone(), resolved(member, document)))
                    .collect::<Map<_, _>>(),
            )
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| resolved(item, document)).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
#[path = "person_types_tests.rs"]
mod person_types_tests;
