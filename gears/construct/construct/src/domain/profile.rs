use construct_sdk::gts::{IDENTITY_TYPE, PREFERENCE_TYPE, ROLE_TYPE, SKILL_TYPE};
use serde_json::Value;
use toolkit_macros::domain_model;

use super::plan::NodeKey;

/// A category of a person's profile: one person type in graph storage.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Identity,
    Role,
    Skill,
    Preference,
}

impl Category {
    /// Every category, in the order the planner lists them.
    pub const ALL: [Self; 4] = [Self::Identity, Self::Role, Self::Skill, Self::Preference];

    /// The category whose person type is `type_id`, or `None` for any other type.
    #[must_use]
    pub fn from_type_id(type_id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.type_id() == type_id)
    }

    /// The GTS id of the category's person type.
    #[must_use]
    pub fn type_id(self) -> &'static str {
        match self {
            Self::Identity => IDENTITY_TYPE,
            Self::Role => ROLE_TYPE,
            Self::Skill => SKILL_TYPE,
            Self::Preference => PREFERENCE_TYPE,
        }
    }

    /// The plain name the model sees.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Role => "role",
            Self::Skill => "skill",
            Self::Preference => "preference",
        }
    }
}

/// One stored fact: its node key, its category, its property and its value.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    pub node_key: NodeKey,
    pub category: Category,
    pub property: String,
    pub value: Value,
}

/// A subject's profile as the planner reads it: the version of its root node, none for a subject without a
/// profile yet, and its facts.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub version: Option<u64>,
    pub facts: Vec<Fact>,
}
