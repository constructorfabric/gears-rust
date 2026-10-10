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

/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-client:p1
#[async_trait]
impl<R: NoteRepository + 'static> ConstructClientV1 for LocalClient<R> {
    async fn create_note(
        &self,
        ctx: &SecurityContext,
        note: NewFoundationNote,
    ) -> Result<FoundationNote, CanonicalError> {
        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-create
        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-create-run
        self.service
            .create_note(ctx, note)
            .await
            // @cpt-end:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-create-run
            // @cpt-begin:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-map
            // @cpt-begin:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-map-return
            .map_err(CanonicalError::from)
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-map-return
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-map
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-create
    }

    async fn get_note(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<FoundationNote, CanonicalError> {
        // @cpt-begin:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-get
        self.service
            .get_note(ctx, id)
            .await
            // @cpt-begin:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-map
            // @cpt-begin:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-map-return
            .map_err(CanonicalError::from)
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-map-return
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-map
        // @cpt-end:cpt-cf-construct-flow-gear-foundation-client-note:p1:inst-client-get
    }
}
