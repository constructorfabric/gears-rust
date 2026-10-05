use construct_sdk::models::{FoundationNote, NewFoundationNote};
use uuid::Uuid;

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct FoundationNoteDto {
    #[schema(value_type = String)]
    pub id: Uuid,
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub text: String,
}

impl From<FoundationNote> for FoundationNoteDto {
    fn from(note: FoundationNote) -> Self {
        Self {
            id: note.id,
            tenant_id: note.tenant_id,
            text: note.text,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct CreateFoundationNoteRequest {
    pub text: String,
}

impl From<CreateFoundationNoteRequest> for NewFoundationNote {
    fn from(req: CreateFoundationNoteRequest) -> Self {
        Self::new(req.text)
    }
}
