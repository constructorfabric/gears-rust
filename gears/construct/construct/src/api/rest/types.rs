use crate::domain::service::Service;
use crate::domain::subject_settings::SubjectSettingsService;
use crate::infra::storage::sea_orm_repo::SeaOrmNoteRepository;
use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;

/// Concrete service type used by the REST layer.
pub type ConcreteService = Service<SeaOrmNoteRepository>;

/// Concrete subject settings service used by the REST layer.
pub type ConcreteSettings = SubjectSettingsService<SeaOrmSubjectSettingsRepository>;
