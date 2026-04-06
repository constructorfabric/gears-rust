use super::*;

#[test]
fn body_empty_is_empty() {
    assert!(Body::Empty.is_empty());
}

#[test]
fn body_bytes_not_empty() {
    let body = Body::from(Bytes::from("hello"));
    assert!(!body.is_empty());
}

#[test]
fn empty_bytes_becomes_empty_body() {
    let body = Body::from(Bytes::new());
    assert!(body.is_empty());
}

#[test]
fn from_string() {
    let body = Body::from("hello".to_string());
    assert!(!body.is_empty());
    assert!(matches!(body, Body::Bytes(_)));
}

#[test]
fn from_static_str() {
    let body = Body::from("hello");
    assert!(matches!(body, Body::Bytes(_)));
}

#[test]
fn from_vec() {
    let body = Body::from(vec![1, 2, 3]);
    assert!(matches!(body, Body::Bytes(_)));
}

#[test]
fn from_unit() {
    let body = Body::from(());
    assert!(body.is_empty());
}

#[test]
fn debug_does_not_leak_content() {
    let body = Body::from(Bytes::from("secret-data"));
    let debug = format!("{body:?}");
    assert!(debug.contains("11 bytes"));
    assert!(!debug.contains("secret-data"));
}

#[test]
fn debug_empty() {
    assert_eq!(format!("{:?}", Body::Empty), "Body::Empty");
}

#[test]
fn debug_stream() {
    let stream: BodyStream = Box::pin(futures_util::stream::empty());
    let body = Body::Stream(stream);
    assert_eq!(format!("{body:?}"), "Body::Stream(...)");
}

#[tokio::test]
async fn into_bytes_from_empty() {
    let bytes = Body::Empty.into_bytes().await.unwrap();
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn into_bytes_from_bytes() {
    let original = Bytes::from("hello");
    let bytes = Body::Bytes(original.clone()).into_bytes().await.unwrap();
    assert_eq!(bytes, original);
}

#[tokio::test]
async fn into_bytes_from_stream() {
    let chunks = vec![Ok(Bytes::from("hel")), Ok(Bytes::from("lo"))];
    let stream: BodyStream = Box::pin(futures_util::stream::iter(chunks));
    let bytes = Body::Stream(stream).into_bytes().await.unwrap();
    assert_eq!(bytes, Bytes::from("hello"));
}

#[test]
fn try_into_bytes_succeeds() {
    let body = Body::Bytes(Bytes::from("data"));
    assert_eq!(body.try_into_bytes().unwrap(), Bytes::from("data"));
}

#[test]
fn try_into_bytes_fails_on_empty() {
    let body = Body::Empty;
    assert!(body.try_into_bytes().is_err());
}

#[test]
fn try_into_stream_fails_on_bytes() {
    let body = Body::Bytes(Bytes::from("data"));
    assert!(body.try_into_stream().is_err());
}
