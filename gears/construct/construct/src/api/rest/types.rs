use crate::domain::record_intake::RecordIntakeService;
use crate::infra::storage::record_ids_repo::SeaOrmRecordIdRepository;
use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;

/// Concrete intake service type used by the REST layer.
pub type ConcreteIntake =
    RecordIntakeService<SeaOrmRecordIdRepository, SeaOrmSubjectSettingsRepository>;
