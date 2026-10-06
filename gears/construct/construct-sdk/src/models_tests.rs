use super::*;

#[test]
fn new_note_input_accepts_str_and_string() {
    assert_eq!(
        NewFoundationNote::new("hello"),
        NewFoundationNote::new(String::from("hello")),
    );
}
