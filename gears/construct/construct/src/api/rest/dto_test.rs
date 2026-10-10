use construct_sdk::models::{FoundationNote, NewFoundationNote};
use uuid::Uuid;

use super::dto::{CreateFoundationNoteRequest, FoundationNoteDto};

#[test]
fn note_converts_to_dto_field_for_field() {
    let note = FoundationNote::new(Uuid::new_v4(), Uuid::new_v4(), "hello".to_owned());

    let dto = FoundationNoteDto::from(note.clone());

    assert_eq!(dto.id, note.id);
    assert_eq!(dto.tenant_id, note.tenant_id);
    assert_eq!(dto.text, note.text);
}

#[test]
fn create_request_converts_to_new_note() {
    let req = CreateFoundationNoteRequest {
        text: "hello".to_owned(),
    };

    let new = NewFoundationNote::from(req);

    assert_eq!(new.text, "hello");
}
