use super::*;

#[test]
fn serialize_data_only() {
    let event = ServerEvent {
        data: "hello".into(),
        ..Default::default()
    };
    let bytes = serialize_event(&event);
    assert_eq!(bytes.as_ref(), b"data: hello\n\n");
}

#[test]
fn serialize_all_fields() {
    let event = ServerEvent {
        id: Some("42".into()),
        event: Some("update".into()),
        data: "payload".into(),
        retry: Some(3000),
    };
    let bytes = serialize_event(&event);
    let expected = "id: 42\nevent: update\nretry: 3000\ndata: payload\n\n";
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), expected);
}

#[test]
fn serialize_multiline_data() {
    let event = ServerEvent {
        data: "line1\nline2\nline3".into(),
        ..Default::default()
    };
    let bytes = serialize_event(&event);
    let expected = "data: line1\ndata: line2\ndata: line3\n\n";
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), expected);
}
