use construct_sdk::models::FoundationNote;

use super::entity;

impl From<entity::Model> for FoundationNote {
    fn from(entity: entity::Model) -> Self {
        Self::new(entity.id, entity.tenant_id, entity.text)
    }
}
