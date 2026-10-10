use super::subject_settings_entity;
use crate::domain::subject_settings::SubjectSettings;

impl From<subject_settings_entity::Model> for SubjectSettings {
    fn from(row: subject_settings_entity::Model) -> Self {
        Self {
            personalization_enabled: row.personalization_enabled,
            erasure_in_progress: row.erasure_in_progress,
        }
    }
}
