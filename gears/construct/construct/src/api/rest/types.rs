use crate::domain::record_intake::RecordIntakeService;
use crate::domain::subject_settings::SubjectSettingsService;
use crate::infra::storage::record_ids_repo::SeaOrmRecordIdRepository;
use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;

/// Concrete intake service type used by the REST layer.
pub type ConcreteIntake =
    RecordIntakeService<SeaOrmRecordIdRepository, SeaOrmSubjectSettingsRepository>;

/// Concrete subject settings service used by the REST layer.
pub type ConcreteSettings = SubjectSettingsService<SeaOrmSubjectSettingsRepository>;
