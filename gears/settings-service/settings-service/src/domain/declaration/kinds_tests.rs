use super::*;

#[test]
fn strictness_is_the_declaration_order() {
    assert!(DataClassification::Public < DataClassification::Pii);
    assert!(DataClassification::Pii < DataClassification::Secret);
}

#[test]
fn classification_round_trips_through_its_wire_string() {
    for class in [
        DataClassification::Public,
        DataClassification::Pii,
        DataClassification::Secret,
    ] {
        assert_eq!(class.as_str().parse::<DataClassification>(), Ok(class));
        assert_eq!(DataClassification::from_stored(class.as_str()), class);
    }
    assert_eq!("restricted".parse::<DataClassification>(), Err(UnknownKind));
    assert_eq!("".parse::<DataClassification>(), Err(UnknownKind));
    assert_eq!("PII".parse::<DataClassification>(), Err(UnknownKind));
}

#[test]
fn an_unknown_stored_classification_fails_closed() {
    assert_eq!(
        DataClassification::from_stored("restricted"),
        DataClassification::Secret
    );
    assert_eq!(
        DataClassification::from_stored(""),
        DataClassification::Secret
    );
}

#[test]
fn status_round_trips_through_its_wire_string() {
    for status in [DeclarationStatus::Active, DeclarationStatus::Retired] {
        assert_eq!(status.as_str().parse::<DeclarationStatus>(), Ok(status));
        assert_eq!(DeclarationStatus::from_stored(status.as_str()), status);
    }
    assert_eq!("gone".parse::<DeclarationStatus>(), Err(UnknownKind));
    assert!(DeclarationStatus::Active.is_active());
    assert!(!DeclarationStatus::Active.is_retired());
    assert!(DeclarationStatus::Retired.is_retired());
}

#[test]
fn an_unknown_stored_status_is_retired() {
    assert_eq!(
        DeclarationStatus::from_stored("gone"),
        DeclarationStatus::Retired
    );
}
