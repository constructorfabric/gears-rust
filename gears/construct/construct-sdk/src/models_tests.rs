use super::*;

#[test]
fn new_note_input_accepts_str_and_string() {
    assert_eq!(
        NewFoundationNote::new("hello"),
        NewFoundationNote::new(String::from("hello")),
    );
}

#[test]
fn note_keeps_the_given_id_tenant_and_text() {
    let (id, tenant_id) = (Uuid::new_v4(), Uuid::new_v4());

    let note = FoundationNote::new(id, tenant_id, "hello".to_owned());

    assert_eq!(note.id, id);
    assert_eq!(note.tenant_id, tenant_id);
    assert_eq!(note.text, "hello");
}
