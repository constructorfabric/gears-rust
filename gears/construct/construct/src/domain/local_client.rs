use std::sync::Arc;

use async_trait::async_trait;
use construct_sdk::{ConstructClientV1, FoundationNote, NewFoundationNote};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::repo::NoteRepository;
use crate::domain::service::Service;

#[domain_model]
pub struct LocalClient<R: NoteRepository + 'static> {
    service: Arc<Service<R>>,
}

impl<R: NoteRepository + 'static> LocalClient<R> {
    #[must_use]
    pub fn new(service: Arc<Service<R>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl<R: NoteRepository + 'static> ConstructClientV1 for LocalClient<R> {
    async fn create_note(
        &self,
        ctx: &SecurityContext,
        note: NewFoundationNote,
    ) -> Result<FoundationNote, CanonicalError> {
        self.service
            .create_note(ctx, note)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_note(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<FoundationNote, CanonicalError> {
        self.service
            .get_note(ctx, id)
            .await
            .map_err(CanonicalError::from)
    }
}
