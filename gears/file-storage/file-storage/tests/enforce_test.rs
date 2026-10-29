//! Policy and quota enforcement in `FileService`: MIME, size and metadata limits, quota gating.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::domain::policy::{MetadataLimits, PolicyBody, PolicyScope, SizeLimits};
use file_storage::domain::policy_service::PolicyService;
use file_storage::infra::external_clients::{QuotaClient, QuotaDecision};
use file_storage_sdk::{CustomMetadataEntry, CustomMetadataPatch, NewFile};

mod common;
use common::{Stack, StackBuilder, ctx};

struct CappedQuota {
    cap: u64,
    seen: AtomicU64,
}

impl CappedQuota {
    fn new(cap: u64) -> Self {
        Self {
            cap,
            seen: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl QuotaClient for CappedQuota {
    async fn check_storage_quota(
        &self,
        _tenant_id: Uuid,
        _owner_id: Uuid,
        additional_bytes: u64,
        _metric_name: &str,
    ) -> Result<QuotaDecision, DomainError> {
        let total = self.seen.fetch_add(additional_bytes, Ordering::SeqCst) + additional_bytes;
        if total > self.cap {
            Ok(QuotaDecision::Denied {
                reason: format!("would use {total} > cap {}", self.cap),
            })
        } else {
            Ok(QuotaDecision::Allowed)
        }
    }
}

struct ErroringQuota;

#[async_trait]
impl QuotaClient for ErroringQuota {
    async fn check_storage_quota(
        &self,
        _tenant_id: Uuid,
        _owner_id: Uuid,
        _additional_bytes: u64,
        _metric_name: &str,
    ) -> Result<QuotaDecision, DomainError> {
        Err(DomainError::InternalError)
    }
}

fn new_file(owner: Uuid, mime: &str) -> NewFile {
    common::new_file_for(owner, "doc.bin", mime)
}

/// Sets the tenant-scope policy to `body`.
async fn set_tenant_policy(psvc: &PolicyService, ctx: &SecurityContext, body: PolicyBody) {
    psvc.set_policy(ctx, PolicyScope::Tenant, None, body)
        .await
        .unwrap();
}

#[tokio::test]
async fn create_file_with_disallowed_mime_is_rejected() {
    let Stack { svc, psvc, .. } = StackBuilder::new("enforce").build().await;
    let ctx = ctx(Uuid::now_v7());

    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            allowed_mime_types: vec!["image/*".to_owned()],
            ..PolicyBody::default()
        },
    )
    .await;

    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMimeNotAllowed { .. }),
        "got {err:?}"
    );

    svc.create_file(&ctx, new_file(Uuid::now_v7(), "image/png"), None, false)
        .await
        .expect("image/png should be allowed");
}

#[tokio::test]
async fn finalize_oversized_upload_is_rejected() {
    let Stack { svc, psvc, dp, .. } = StackBuilder::new("enforce").build().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();

    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(10),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await;

    let t = svc
        .create_file(&ctx, new_file(owner, "text/plain"), None, false)
        .await
        .unwrap();

    let err = svc
        .finalize_upload(&ctx, t.file_id, t.version_id, 100, vec![0u8; 32])
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::PolicySizeExceeded {
                limit_bytes: 10,
                ..
            }
        ),
        "got {err:?}"
    );

    dp.put_content(
        &ctx,
        t.file_id,
        t.version_id,
        "text/plain",
        Bytes::from_static(b"hello"),
    )
    .await
    .expect("5 bytes within 10-byte cap");
}

/// Rejected on entry; otherwise the DB `CHECK (size >= 0)` would surface as a 500.
#[tokio::test]
async fn finalize_negative_size_is_a_validation_error() {
    let Stack { svc, store, .. } = StackBuilder::new("enforce").build().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();

    let t = svc
        .create_file(&ctx, new_file(owner, "text/plain"), None, false)
        .await
        .unwrap();

    let err = svc
        .finalize_upload(&ctx, t.file_id, t.version_id, -1, vec![0u8; 32])
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "size"),
        "got {err:?}"
    );

    let version = store
        .get_version(t.file_id, t.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(version.status, file_storage_sdk::VersionStatus::Pending);
    assert_eq!(version.size, 0);

    let claims = common::put_claims(
        t.file_id,
        t.version_id,
        common::backend_path(t.file_id, t.version_id),
        false,
    );
    let err = svc
        .finalize_upload_by_token(&claims, -1, vec![0u8; 32])
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "size"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn create_file_with_too_many_metadata_pairs_is_rejected() {
    let Stack { svc, psvc, .. } = StackBuilder::new("enforce").build().await;
    let ctx = ctx(Uuid::now_v7());
    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            metadata_limits: MetadataLimits {
                max_pairs: Some(1),
                ..MetadataLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await;

    let mut nf = new_file(Uuid::now_v7(), "text/plain");
    nf.custom_metadata = vec![
        CustomMetadataEntry {
            key: "a".to_owned(),
            value: "1".to_owned(),
        },
        CustomMetadataEntry {
            key: "b".to_owned(),
            value: "2".to_owned(),
        },
    ];
    let err = svc.create_file(&ctx, nf, None, false).await.unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMetadataExceeded { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn update_metadata_over_limit_is_rejected_on_resulting_total() {
    let Stack { svc, psvc, .. } = StackBuilder::new("enforce").build().await;
    let ctx = ctx(Uuid::now_v7());
    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            metadata_limits: MetadataLimits {
                max_pairs: Some(2),
                ..MetadataLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await;

    let mut nf = new_file(Uuid::now_v7(), "text/plain");
    nf.custom_metadata = vec![CustomMetadataEntry {
        key: "a".to_owned(),
        value: "1".to_owned(),
    }];
    let t = svc.create_file(&ctx, nf, None, false).await.unwrap();

    let patch = CustomMetadataPatch {
        entries: vec![
            ("b".to_owned(), Some("2".to_owned())),
            ("c".to_owned(), Some("3".to_owned())),
        ],
    };
    let err = svc
        .update_metadata(&ctx, t.file_id, patch, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMetadataExceeded { .. }),
        "got {err:?}"
    );

    let patch_ok = CustomMetadataPatch {
        entries: vec![("a".to_owned(), Some("9".to_owned()))],
    };
    svc.update_metadata(&ctx, t.file_id, patch_ok, None)
        .await
        .expect("replacing an existing key stays within the limit");
}

/// Quota cap 10 bytes, policy size cap 100: the create preflight of 100 busts the quota.
#[tokio::test]
async fn quota_exceeded_rejects_create_when_client_present() {
    let quota: Arc<dyn QuotaClient> = Arc::new(CappedQuota::new(10));
    let Stack { svc, psvc, .. } = StackBuilder::new("enforce").quota(quota).build().await;
    let ctx = ctx(Uuid::now_v7());
    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(100),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await;

    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::QuotaExceeded { .. }),
        "got {err:?}"
    );
}

/// Quota cap 100, size cap 60: create preflights 60, `presign_version` another 60 (120 > 100).
#[tokio::test]
async fn quota_gates_version_creation_not_just_first_upload() {
    let quota: Arc<dyn QuotaClient> = Arc::new(CappedQuota::new(100));
    let Stack { svc, psvc, .. } = StackBuilder::new("enforce").quota(quota).build().await;
    let ctx = ctx(Uuid::now_v7());
    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(60),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await;

    let t = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .expect("first create within quota");

    let err = svc.presign_version(&ctx, t.file_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::QuotaExceeded { .. }),
        "version creation must also be quota-gated, got {err:?}"
    );
}

#[tokio::test]
async fn quota_client_error_fails_closed() {
    let quota: Arc<dyn QuotaClient> = Arc::new(ErroringQuota);
    let Stack { svc, .. } = StackBuilder::new("enforce").quota(quota).build().await;
    let ctx = ctx(Uuid::now_v7());

    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::InternalError),
        "quota client error must fail closed, got {err:?}"
    );
}

/// Without a policy or a quota client nothing is limited: 50 metadata pairs of 100 bytes and a
/// 1 MB upload are stored in full.
#[tokio::test]
async fn no_policy_and_no_quota_is_fully_permissive() {
    let Stack { svc, dp, store, .. } = StackBuilder::new("enforce").build().await;
    let ctx = ctx(Uuid::now_v7());

    let mut nf = new_file(Uuid::now_v7(), "application/x-anything");
    nf.custom_metadata = (0..50)
        .map(|i| CustomMetadataEntry {
            key: format!("k{i}"),
            value: "v".repeat(100),
        })
        .collect();
    let t = svc
        .create_file(&ctx, nf, None, false)
        .await
        .expect("permissive create");

    let big = Bytes::from(vec![0u8; 1_000_000]);
    dp.put_content(&ctx, t.file_id, t.version_id, "application/x-anything", big)
        .await
        .expect("no size limit without policy");

    let (_, metadata) = svc.get_file_with_metadata(&ctx, t.file_id).await.unwrap();
    assert_eq!(metadata.len(), 50);
    let version = store
        .get_version(t.file_id, t.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.size, 1_000_000);
}
