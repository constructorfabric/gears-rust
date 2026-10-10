use async_trait::async_trait;
use construct_sdk::person_types::{IDENTITY_TYPE, PREFERENCES_TYPE, ROLES_TYPE, SKILLS_TYPE};
use serde_json::Value;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::error::DomainError;

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Identity,
    Roles,
    Skills,
    Preferences,
}

impl Category {
    #[must_use]
    pub fn from_type_id(type_id: &str) -> Option<Self> {
        [Self::Identity, Self::Roles, Self::Skills, Self::Preferences]
            .into_iter()
            .find(|category| category.type_id() == type_id)
    }

    #[must_use]
    pub fn type_id(self) -> &'static str {
        match self {
            Self::Identity => IDENTITY_TYPE,
            Self::Roles => ROLES_TYPE,
            Self::Skills => SKILLS_TYPE,
            Self::Preferences => PREFERENCES_TYPE,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Roles => "roles",
            Self::Skills => "skills",
            Self::Preferences => "preferences",
        }
    }
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileVersion(pub i64);

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    pub node_key: String,
    pub category: Category,
    pub property: String,
    pub value: Value,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub version: ProfileVersion,
    pub facts: Vec<Fact>,
}

/// @cpt-dod:cpt-cf-construct-dod-planner-profile-read:p1
#[async_trait]
pub trait ProfileSource: Send + Sync {
    async fn read(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        subject_id: Uuid,
    ) -> Result<Profile, DomainError>;
}
