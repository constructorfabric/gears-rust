use async_trait::async_trait;
use construct_sdk::gts::{IDENTITY_TYPE, PREFERENCE_TYPE, ROLE_TYPE, SKILL_TYPE};
use serde_json::Value;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::error::DomainError;
use super::plan::{NodeKey, Plan};

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

    /// The category whose plain name, the entity kind a plan step carries, is
    /// `name`, or `None` for any other name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.name() == name)
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

/// Reads a subject's profile and its version from graph storage.
#[async_trait]
pub trait ProfileSource: Send + Sync {
    /// # Errors
    ///
    /// When the profile cannot be read. The record is then dropped.
    async fn read(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        subject_id: Uuid,
    ) -> Result<Profile, DomainError>;
}

/// What writing an admitted plan gave.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    Stored,
    /// The profile changed after the plan was built on it, and nothing was stored.
    Conflict,
}

/// Stores an admitted plan in one write, expecting the profile version the plan was built on.
#[async_trait]
pub trait PlanWriter: Send + Sync {
    /// # Errors
    ///
    /// When the write cannot run. The record is then dropped.
    async fn write(&self, ctx: &SecurityContext, plan: &Plan) -> Result<Written, DomainError>;
}
