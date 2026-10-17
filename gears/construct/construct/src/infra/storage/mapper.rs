use construct_sdk::models::FoundationNote;

use super::{entity, subject_settings_entity};
use crate::domain::subject_settings::SubjectSettings;

impl From<entity::Model> for FoundationNote {
    fn from(entity: entity::Model) -> Self {
        Self::new(entity.id, entity.tenant_id, entity.text)
    }
}

impl From<subject_settings_entity::Model> for SubjectSettings {
    fn from(row: subject_settings_entity::Model) -> Self {
        Self {
            personalization_enabled: row.personalization_enabled,
            erasure_in_progress: row.erasure_in_progress,
        }
    }
}
