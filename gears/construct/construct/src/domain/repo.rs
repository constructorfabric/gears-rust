use async_trait::async_trait;
use construct_sdk::models::FoundationNote;
use toolkit::domain::DomainModel;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::error::DomainError;

#[async_trait]
pub trait NoteRepository: Send + Sync
where
    FoundationNote: DomainModel,
{
    /// Store a new note. The scope must cover the note's tenant.
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        note: FoundationNote,
    ) -> Result<FoundationNote, DomainError>;

    /// One note by id, if it is inside the scope.
    async fn find_by_id<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<FoundationNote>, DomainError>;
}
