use crate::domain::service::Service;
use crate::infra::storage::sea_orm_repo::SeaOrmNoteRepository;

/// Concrete service type used by the REST layer.
pub type ConcreteService = Service<SeaOrmNoteRepository>;
