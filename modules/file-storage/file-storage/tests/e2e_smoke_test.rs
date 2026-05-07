//! Smoke check — the harness comes up, a bucket is created, and the
//! Service can list its (single) backend. Fast (~50 ms). The real
//! lifecycle assertions live in `e2e_lifecycle_test.rs`.

mod e2e_common;
use e2e_common::{EnvSpec, make_ctx, make_env};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
async fn s3s_harness_starts_and_service_lists_default_private_backend() {
    let env = make_env(EnvSpec::private_only(vec![
        "upload.s3.multipart.sigv4.v1",
        "download.s3.sigv4.v1",
    ]))
    .await;

    let ctx = make_ctx(Uuid::new_v4());
    let backends = env.service.list_backends(&ctx).await.expect("list_backends");
    assert_eq!(backends.len(), 1);
    let only = &backends[0];
    assert_eq!(only.id, env.default_private_id);
    assert!(only.default_private);
    assert!(!only.default_public);
}
