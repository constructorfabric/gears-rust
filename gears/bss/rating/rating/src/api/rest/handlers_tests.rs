use axum::http::{HeaderMap, HeaderValue};

use super::{MAX_REQUEST_ID_LEN, request_id};

#[test]
fn the_request_id_is_recorded_when_it_is_short() {
    let mut headers = HeaderMap::new();
    assert_eq!(request_id(&headers), None);
    headers.insert("x-request-id", HeaderValue::from_static("req-1"));
    assert_eq!(request_id(&headers).as_deref(), Some("req-1"));
    headers.insert(
        "x-request-id",
        HeaderValue::from_str(&"r".repeat(MAX_REQUEST_ID_LEN + 1)).unwrap(),
    );
    assert_eq!(request_id(&headers), None);
}

#[test]
fn only_an_allowlisted_request_id_reaches_the_audit_row() {
    let mut headers = HeaderMap::new();
    for kept in ["0b7c4b1e-6f0a-4c39-9a51-6a4c1e2f3d4e", "trace_01.a-B"] {
        headers.insert("x-request-id", HeaderValue::from_str(kept).unwrap());
        assert_eq!(request_id(&headers).as_deref(), Some(kept));
    }
    // Spaces and a tab pass `HeaderValue::to_str`; they are dropped all the same.
    for dropped in ["req 1", "req\t1", "req;drop", "r\u{e9}q"] {
        let Ok(value) = HeaderValue::from_str(dropped) else {
            continue;
        };
        headers.insert("x-request-id", value);
        assert_eq!(request_id(&headers), None, "{dropped:?}");
    }
}
