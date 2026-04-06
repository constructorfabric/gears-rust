use super::*;

async fn collect_body(body: Body) -> Vec<u8> {
    axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn json_serialize_sets_content_type() {
    #[derive(Serialize)]
    struct Payload {
        name: String,
    }

    let (body, ct) = Json(Payload {
        name: "test".into(),
    })
    .into_body();

    assert_eq!(ct.unwrap().as_bytes(), b"application/json");
    let bytes = collect_body(body).await;
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["name"], "test");
}

#[tokio::test]
async fn json_value_sets_content_type() {
    let (body, ct) = serde_json::json!({"key": "val"}).into_body();

    assert_eq!(ct.unwrap().as_bytes(), b"application/json");
    let bytes = collect_body(body).await;
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["key"], "val");
}

#[tokio::test]
async fn str_has_no_content_type() {
    let (body, ct) = "hello".into_body();

    assert!(ct.is_none());
    let bytes = collect_body(body).await;
    assert_eq!(bytes, b"hello");
}

#[tokio::test]
async fn string_has_no_content_type() {
    let (body, ct) = String::from("hello").into_body();

    assert!(ct.is_none());
    let bytes = collect_body(body).await;
    assert_eq!(bytes, b"hello");
}

#[tokio::test]
async fn bytes_has_no_content_type() {
    let (body, ct) = bytes::Bytes::from_static(b"raw").into_body();

    assert!(ct.is_none());
    let bytes = collect_body(body).await;
    assert_eq!(bytes, b"raw");
}

#[tokio::test]
async fn vec_u8_has_no_content_type() {
    let (body, ct) = vec![1u8, 2, 3].into_body();

    assert!(ct.is_none());
    let bytes = collect_body(body).await;
    assert_eq!(bytes, &[1, 2, 3]);
}
