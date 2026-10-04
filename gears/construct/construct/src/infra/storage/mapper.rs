use construct_sdk::models::FoundationNote;

use super::entity;

impl From<entity::Model> for FoundationNote {
    fn from(entity: entity::Model) -> Self {
        Self {
            id: entity.id,
            tenant_id: entity.tenant_id,
            text: entity.text,
        }
    }
}
