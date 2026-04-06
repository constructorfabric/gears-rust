use super::*;

// -- Task 2.4: exact wire bytes for single text field -----------------------

#[test]
fn buffered_single_text_field() {
    let body = MultipartBody::with_boundary("BOUND")
        .unwrap()
        .text("name", "Alice")
        .into_body();
    let bytes = match body {
        Body::Bytes(b) => b,
        _ => panic!("expected Body::Bytes"),
    };
    let expected = "--BOUND\r\n\
        Content-Disposition: form-data; name=\"name\"\r\n\
        \r\n\
        Alice\r\n\
        --BOUND--\r\n";
    assert_eq!(bytes.as_ref(), expected.as_bytes());
}

// -- Task 5.1: Part constructors --------------------------------------------

#[test]
fn part_text_fields() {
    let p = Part::text("purpose", "fine-tune");
    assert_eq!(p.name(), "purpose");
    assert_eq!(p.get_filename(), None);
    assert_eq!(p.get_content_type(), None);
    assert!(!p.is_streaming());
}

#[test]
fn part_bytes_fields() {
    let p = Part::bytes("file", vec![0x89, 0x50, 0x4E, 0x47]);
    assert_eq!(p.name(), "file");
    assert!(!p.is_streaming());
}

#[test]
fn part_stream_is_streaming() {
    let stream: BodyStream = Box::pin(futures_util::stream::empty());
    let p = Part::stream("file", stream);
    assert_eq!(p.name(), "file");
    assert!(p.is_streaming());
    assert_eq!(p.get_filename(), None);
}

#[test]
fn part_chained_setters() {
    let p = Part::bytes("file", vec![1, 2, 3])
        .filename("photo.jpg")
        .content_type("image/jpeg");
    assert_eq!(p.get_filename(), Some("photo.jpg"));
    assert_eq!(p.get_content_type(), Some("image/jpeg"));
}

// -- Task 5.2: MultipartBody::new() boundary --------------------------------

#[test]
fn new_produces_32_char_hex_boundary() {
    let mb = MultipartBody::new();
    assert_eq!(mb.boundary.len(), 32);
    assert!(mb.boundary.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn two_builders_different_boundaries() {
    let a = MultipartBody::new();
    let b = MultipartBody::new();
    assert_ne!(a.boundary, b.boundary);
}

// -- Task 5.3: with_boundary validation -------------------------------------

#[test]
fn with_boundary_valid() {
    assert!(MultipartBody::with_boundary("test-boundary-123").is_ok());
}

#[test]
fn with_boundary_too_long() {
    let long = "a".repeat(71);
    assert!(MultipartBody::with_boundary(long).is_err());
}

#[test]
fn with_boundary_null_byte() {
    assert!(MultipartBody::with_boundary("bad\x00boundary").is_err());
}

// -- Task 5.4: content_type format ------------------------------------------

#[test]
fn content_type_format() {
    let b = MultipartBody::with_boundary("abc123").unwrap();
    assert_eq!(b.content_type(), "multipart/form-data; boundary=abc123");
}

#[test]
fn content_type_header_value_valid() {
    let b = MultipartBody::with_boundary("abc123").unwrap();
    let hv = b.content_type_header_value();
    assert_eq!(hv.to_str().unwrap(), "multipart/form-data; boundary=abc123");
}

// -- Task 5.5: buffered serialization ---------------------------------------

#[test]
fn buffered_multi_part_with_filename_and_content_type() {
    let body = MultipartBody::with_boundary("B")
        .unwrap()
        .text("model", "gpt-4")
        .part(
            Part::bytes("file", &b"PDF-DATA"[..])
                .filename("doc.pdf")
                .content_type("application/pdf"),
        )
        .into_body();
    let bytes = match body {
        Body::Bytes(b) => b,
        _ => panic!("expected Body::Bytes"),
    };
    let expected = "--B\r\n\
        Content-Disposition: form-data; name=\"model\"\r\n\
        \r\n\
        gpt-4\r\n\
        --B\r\n\
        Content-Disposition: form-data; name=\"file\"; filename=\"doc.pdf\"\r\n\
        Content-Type: application/pdf\r\n\
        \r\n\
        PDF-DATA\r\n\
        --B--\r\n";
    assert_eq!(bytes.as_ref(), expected.as_bytes());
}

#[test]
fn buffered_empty_parts() {
    let body = MultipartBody::with_boundary("B").unwrap().into_body();
    let bytes = match body {
        Body::Bytes(b) => b,
        _ => panic!("expected Body::Bytes"),
    };
    assert_eq!(bytes.as_ref(), b"--B--\r\n");
}

#[test]
fn buffered_quote_escaping_in_filename() {
    let body = MultipartBody::with_boundary("B")
        .unwrap()
        .part(Part::bytes("file", &b"data"[..]).filename("he said \"hello\""))
        .into_body();
    let bytes = match body {
        Body::Bytes(b) => b,
        _ => panic!("expected Body::Bytes"),
    };
    let s = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(s.contains("filename=\"he said \\\"hello\\\"\""));
}

// -- Task 5.6: streaming serialization (async) ------------------------------

#[tokio::test]
async fn streaming_mixed_parts() {
    let stream: BodyStream = Box::pin(futures_util::stream::iter(vec![
        Ok(Bytes::from("chunk1")),
        Ok(Bytes::from("chunk2")),
    ]));
    let body = MultipartBody::with_boundary("B")
        .unwrap()
        .text("model", "whisper-1")
        .part(
            Part::stream("file", stream)
                .filename("audio.mp3")
                .content_type("audio/mpeg"),
        )
        .into_body();
    assert!(matches!(body, Body::Stream(_)));
    let bytes = body.into_bytes().await.unwrap();
    let expected = "--B\r\n\
        Content-Disposition: form-data; name=\"model\"\r\n\
        \r\n\
        whisper-1\r\n\
        --B\r\n\
        Content-Disposition: form-data; name=\"file\"; filename=\"audio.mp3\"\r\n\
        Content-Type: audio/mpeg\r\n\
        \r\n\
        chunk1chunk2\r\n\
        --B--\r\n";
    assert_eq!(bytes.as_ref(), expected.as_bytes());
}

#[tokio::test]
async fn streaming_chunk_ordering() {
    let stream: BodyStream = Box::pin(futures_util::stream::iter(vec![
        Ok(Bytes::from("a")),
        Ok(Bytes::from("b")),
        Ok(Bytes::from("c")),
    ]));
    let body = MultipartBody::with_boundary("B")
        .unwrap()
        .part(Part::stream("data", stream))
        .into_body();
    let bytes = body.into_bytes().await.unwrap();
    let s = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(s.contains("abc\r\n--B--\r\n"));
}

#[tokio::test]
async fn streaming_error_propagated() {
    let stream: BodyStream = Box::pin(futures_util::stream::iter(vec![
        Ok(Bytes::from("ok")),
        Err(Box::new(std::io::Error::other("boom")) as BoxError),
    ]));
    let body = MultipartBody::with_boundary("B")
        .unwrap()
        .part(Part::stream("data", stream))
        .into_body();
    let result = body.into_bytes().await;
    assert!(result.is_err());
}

// -- Task 5.7: into_request -------------------------------------------------

#[test]
fn into_request_correct_fields() {
    let req = MultipartBody::with_boundary("B")
        .unwrap()
        .text("key", "val")
        .into_request("POST", "/upload")
        .unwrap();
    assert_eq!(req.method(), http::Method::POST);
    assert_eq!(req.uri(), "/upload");
    assert_eq!(
        req.headers().get("content-type").unwrap(),
        "multipart/form-data; boundary=B"
    );
    assert!(matches!(req.into_body(), Body::Bytes(_)));
}

// -- Task 5.8: From<MultipartBody> for Body ---------------------------------

#[test]
fn from_multipart_body_matches_into_body() {
    let a = MultipartBody::with_boundary("B")
        .unwrap()
        .text("k", "v")
        .into_body();
    let b: Body = MultipartBody::with_boundary("B")
        .unwrap()
        .text("k", "v")
        .into();
    let a_bytes = match a {
        Body::Bytes(b) => b,
        _ => panic!("expected bytes"),
    };
    let b_bytes = match b {
        Body::Bytes(b) => b,
        _ => panic!("expected bytes"),
    };
    assert_eq!(a_bytes, b_bytes);
}

// -- Scenario gap coverage --------------------------------------------------

#[test]
fn part_text_empty_string() {
    let p = Part::text("field", "");
    assert!(!p.is_streaming());
    match &p.body {
        PartBody::Bytes(b) => assert!(b.is_empty()),
        _ => panic!("expected Bytes"),
    }
}

#[test]
fn part_stream_filename_setter() {
    let stream: BodyStream = Box::pin(futures_util::stream::empty());
    let p = Part::stream("file", stream).filename("audio.mp3");
    assert!(p.is_streaming());
    assert_eq!(p.get_filename(), Some("audio.mp3"));
}

#[test]
fn has_streaming_parts_false_for_buffered() {
    let mb = MultipartBody::new().text("a", "1").bytes("b", &b"data"[..]);
    assert!(!mb.has_streaming_parts());
}

#[test]
fn has_streaming_parts_true_for_mixed() {
    let stream: BodyStream = Box::pin(futures_util::stream::empty());
    let mb = MultipartBody::new()
        .text("a", "1")
        .part(Part::stream("file", stream));
    assert!(mb.has_streaming_parts());
}

#[test]
fn into_request_invalid_uri() {
    let result = MultipartBody::new()
        .text("k", "v")
        .into_request("POST", "not a valid uri \0");
    assert!(result.is_err());
}

#[test]
fn multipart_error_display_includes_reason() {
    let err = MultipartError::InvalidBoundary {
        reason: "too long".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("too long"), "got: {msg}");
}

#[test]
fn multipart_error_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MultipartError>();
}

#[test]
fn text_shorthand_matches_explicit_part() {
    let a = MultipartBody::with_boundary("B")
        .unwrap()
        .text("key", "val")
        .into_body();
    let b = MultipartBody::with_boundary("B")
        .unwrap()
        .part(Part::text("key", "val"))
        .into_body();
    let a = match a {
        Body::Bytes(b) => b,
        _ => panic!(),
    };
    let b = match b {
        Body::Bytes(b) => b,
        _ => panic!(),
    };
    assert_eq!(a, b);
}

#[test]
fn bytes_shorthand_matches_explicit_part() {
    let a = MultipartBody::with_boundary("B")
        .unwrap()
        .bytes("f", &b"data"[..])
        .into_body();
    let b = MultipartBody::with_boundary("B")
        .unwrap()
        .part(Part::bytes("f", &b"data"[..]))
        .into_body();
    let a = match a {
        Body::Bytes(b) => b,
        _ => panic!(),
    };
    let b = match b {
        Body::Bytes(b) => b,
        _ => panic!(),
    };
    assert_eq!(a, b);
}

// -- CRLF injection prevention ------------------------------------------------

#[test]
fn crlf_stripped_from_part_name() {
    let body = MultipartBody::with_boundary("B")
        .unwrap()
        .part(Part::text("field\r\nEvil-Header: injected", "val"))
        .into_body();
    let bytes = match body {
        Body::Bytes(b) => b,
        _ => panic!("expected Body::Bytes"),
    };
    let s = String::from_utf8(bytes.to_vec()).unwrap();
    // CR/LF stripped — the injected text is folded into the quoted name value,
    // not emitted as a separate header line.
    assert!(
        !s.contains("\r\nEvil-Header:"),
        "CRLF injection in name: {s}"
    );
    assert!(s.contains("name=\"fieldEvil-Header: injected\""));
}

#[test]
fn crlf_stripped_from_filename() {
    let body = MultipartBody::with_boundary("B")
        .unwrap()
        .part(Part::bytes("file", &b"data"[..]).filename("bad\r\nX-Injected: yes"))
        .into_body();
    let bytes = match body {
        Body::Bytes(b) => b,
        _ => panic!("expected Body::Bytes"),
    };
    let s = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        !s.contains("\r\nX-Injected:"),
        "CRLF injection in filename: {s}"
    );
    assert!(s.contains("filename=\"badX-Injected: yes\""));
}

#[test]
fn crlf_stripped_from_content_type() {
    let body = MultipartBody::with_boundary("B")
        .unwrap()
        .part(Part::bytes("file", &b"data"[..]).content_type("text/plain\r\nX-Injected: yes"))
        .into_body();
    let bytes = match body {
        Body::Bytes(b) => b,
        _ => panic!("expected Body::Bytes"),
    };
    let s = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        !s.contains("\r\nX-Injected:"),
        "CRLF injection in content_type: {s}"
    );
    assert!(s.contains("Content-Type: text/plainX-Injected: yes\r\n"));
}
