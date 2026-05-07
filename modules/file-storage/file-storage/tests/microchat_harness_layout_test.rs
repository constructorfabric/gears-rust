//! Exploratory test that pins down the s3s-fs on-disk layout
//! assumption baked into the microchat harness helpers
//! (`object_path`, `object_exists`, `read_object_from_disk`,
//! `list_object_keys`, `list_all_entries`) and the CopyObject counter
//! wired through `CountedS3Service`.
//!
//! If s3s-fs ever changes its disk layout, this test fails with a
//! useful diff before any of the lifecycle / race tests do.

mod e2e_common;

use e2e_common::{EnvSpec, copy_object_count, list_all_entries, list_object_keys, make_env};

#[tokio::test(flavor = "current_thread")]
async fn s3s_fs_writes_objects_at_root_bucket_key_layout() {
    let env = make_env(EnvSpec::priv_pub()).await;
    let h = &env.buckets["priv"];

    // Write a single object via the AWS SDK (path-style) to confirm
    // the disk path s3s-fs picks for a `put_object`.
    let creds = aws_credential_types::Credentials::new(
        h.access_key.clone(),
        h.secret_key.clone(),
        None,
        None,
        "test",
    );
    let cfg = aws_sdk_s3::config::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_config::Region::new("us-east-1"))
        .endpoint_url(h.s3_endpoint.clone())
        .credentials_provider(aws_sdk_s3::config::SharedCredentialsProvider::new(creds))
        .force_path_style(true)
        .build();
    let client = aws_sdk_s3::Client::from_conf(cfg);

    client
        .put_object()
        .bucket(&h.bucket)
        .key("f/abcdef0123")
        .body(aws_sdk_s3::primitives::ByteStream::from_static(b"hello"))
        .send()
        .await
        .expect("put_object");

    // Layout assertion: object is at <root>/<bucket>/f/abcdef0123.
    let on_disk = h.root_path.join("f").join("abcdef0123");
    assert!(
        on_disk.exists(),
        "expected object at {} (s3s-fs layout assumption)",
        on_disk.display()
    );
    assert_eq!(
        std::fs::read(&on_disk).unwrap(),
        b"hello",
        "object bytes match what we PUT"
    );

    // list_object_keys should return exactly ["f/abcdef0123"].
    assert_eq!(list_object_keys(&env, "priv"), vec!["f/abcdef0123"]);

    // list_all_entries scans the server root — anything outside the
    // bucket subdir (sidecar metadata files, internal-info files) is
    // also returned. We just assert that the bucket-relative path
    // appears verbatim somewhere in the list.
    let all = list_all_entries(&env, "priv");
    assert!(
        all.iter()
            .any(|p| p.ends_with("f/abcdef0123") || p == &format!("{}/f/abcdef0123", h.bucket)),
        "list_all_entries should include the new object; got {all:?}"
    );

    // No CopyObject yet.
    assert_eq!(copy_object_count(&env, "priv"), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn copy_object_counter_increments_on_real_copy_request() {
    let env = make_env(EnvSpec::priv_pub()).await;
    let h = &env.buckets["priv"];

    let creds = aws_credential_types::Credentials::new(
        h.access_key.clone(),
        h.secret_key.clone(),
        None,
        None,
        "test",
    );
    let cfg = aws_sdk_s3::config::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_config::Region::new("us-east-1"))
        .endpoint_url(h.s3_endpoint.clone())
        .credentials_provider(aws_sdk_s3::config::SharedCredentialsProvider::new(creds))
        .force_path_style(true)
        .build();
    let client = aws_sdk_s3::Client::from_conf(cfg);

    client
        .put_object()
        .bucket(&h.bucket)
        .key("src.txt")
        .body(aws_sdk_s3::primitives::ByteStream::from_static(b"abc"))
        .send()
        .await
        .expect("put_object");

    assert_eq!(
        copy_object_count(&env, "priv"),
        0,
        "PUT (no x-amz-copy-source) must not tick the CopyObject counter"
    );

    client
        .copy_object()
        .copy_source(format!("{}/src.txt", h.bucket))
        .bucket(&h.bucket)
        .key("dst.txt")
        .send()
        .await
        .expect("copy_object");

    assert_eq!(
        copy_object_count(&env, "priv"),
        1,
        "CopyObject should tick the counter exactly once"
    );

    client
        .copy_object()
        .copy_source(format!("{}/src.txt", h.bucket))
        .bucket(&h.bucket)
        .key("dst2.txt")
        .send()
        .await
        .expect("copy_object 2");

    assert_eq!(copy_object_count(&env, "priv"), 2);

    // Counter is per-server, not per-bucket: the `pub` server has not
    // received any CopyObject, so its counter is still zero.
    assert_eq!(copy_object_count(&env, "pub"), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_private_only_env_still_constructs_one_bucket() {
    // Smoke-equivalent: the legacy single-bucket `EnvSpec::private_only`
    // shape used by `e2e_smoke_test.rs` keeps working post-refactor.
    let env = make_env(EnvSpec::private_only(vec![
        "upload.s3.multipart.sigv4.v1",
        "download.s3.sigv4.v1",
    ]))
    .await;
    assert_eq!(env.buckets.len(), 1);
    assert!(env.buckets.contains_key("priv"));
    assert_eq!(env.buckets["priv"].backend_id, env.default_private_id);
    assert!(env.default_public_id.is_none());
}
