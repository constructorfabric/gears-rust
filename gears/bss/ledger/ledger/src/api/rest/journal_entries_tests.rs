//! Unit tests for the `PATCH …/annotation` request DTO + the handler-side
//! target-kind parse (the DB write is an integration concern, exercised in
//! `tests/postgres_entry_annotation.rs`).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use crate::api::rest::dto::EntryAnnotationRequestDto;
use crate::infra::annotation::AnnotationTarget;

#[test]
fn annotation_dto_deserializes_description() {
    let body = serde_json::json!({
        "description": "ops note",
        "reason": "audit reason",
    });
    let dto: EntryAnnotationRequestDto = serde_json::from_value(body).unwrap();
    assert_eq!(dto.description.as_deref(), Some("ops note"));
    let kind_literal = dto.target_kind.as_deref().unwrap_or("ENTRY");
    assert_eq!(
        AnnotationTarget::parse(kind_literal).unwrap(),
        AnnotationTarget::Entry
    );
}

#[test]
fn annotation_dto_null_description_clears() {
    let body = serde_json::json!({ "description": null, "reason": "clear it" });
    let dto: EntryAnnotationRequestDto = serde_json::from_value(body).unwrap();
    assert_eq!(dto.description, None);
}

#[test]
fn annotation_dto_line_target_parses() {
    let line = uuid::Uuid::now_v7();
    let body = serde_json::json!({
        "description": "line note",
        "target_kind": "LINE",
        "target_line_id": line,
        "reason": "audit reason",
    });
    let dto: EntryAnnotationRequestDto = serde_json::from_value(body).unwrap();
    assert_eq!(dto.target_line_id, Some(line));
    assert_eq!(
        AnnotationTarget::parse(dto.target_kind.as_deref().unwrap()).unwrap(),
        AnnotationTarget::Line
    );
}

#[test]
fn annotation_dto_rejects_bogus_kind() {
    assert!(matches!(
        AnnotationTarget::parse("BOGUS"),
        Err(crate::domain::error::DomainError::InvalidRequest(_))
    ));
}
