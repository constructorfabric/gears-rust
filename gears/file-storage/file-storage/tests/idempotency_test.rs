//! Idempotency-key replay of `create_file`: what a replay re-checks, re-mints or refuses once the
//! recorded state has changed.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::secure::{SecureDeleteExt, SecureUpdateExt};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::domain::policy::{PolicyBody, PolicyScope, SizeLimits};
use file_storage::domain::service::UploadTicket;
use file_storage::infra::storage::entity::file_version::{Column as VerCol, Entity as VerEntity};
use file_storage::infra::storage::entity::idempotency_key::{
    Column as IdemCol, Entity as IdemEntity,
};
use file_storage_sdk::{CustomMetadataEntry, NewFile, OwnerKind};

mod common;
use common::{Stack, StackBuilder, ctx, ctx_for, token_of};

fn new_file() -> NewFile {
    common::new_file("upload.bin")
}

/// Creates (or replays) the keyed file for `owner` as `ctx`.
async fn create_keyed(
    s: &Stack,
    ctx: &SecurityContext,
    owner: Uuid,
    key: &str,
    auto_bind: bool,
) -> Result<UploadTicket, DomainError> {
    s.svc
        .create_file(
            ctx,
            common::new_file_for(owner, "upload.bin", "application/octet-stream"),
            Some(key.to_owned()),
            auto_bind,
        )
        .await
}

/// A replay re-mints a live upload URL, so it re-checks the CURRENT policy: once the MIME is
/// forbidden the retry is rejected, and no second file is made.
#[tokio::test]
async fn idempotent_replay_rechecks_current_mime_policy() {
    let Stack { svc, psvc, dsn, .. } = StackBuilder::new("idempotency").build().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();
    let key = Some("retry-after-policy".to_owned());
    let png = || common::new_file_for(owner, "doc.bin", "image/png");

    svc.create_file(&ctx, png(), key.clone(), false)
        .await
        .expect("first create");

    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            allowed_mime_types: vec!["text/*".to_owned()],
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let err = svc.create_file(&ctx, png(), key, false).await.unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMimeNotAllowed { .. }),
        "got {err:?}"
    );
    assert_eq!(common::raw_count(&dsn, "files").await, 1);
}

/// State that drifts after the key was recorded; the replay must then be refused (never mint a
/// fresh upload token) with the row's own error.
enum Drift {
    OwnershipTransferred,
    StoredBodyCorrupted,
    VersionRowRemoved,
    VersionNoLongerPending,
}

#[tokio::test]
async fn idempotency_replay_is_rejected_after_stored_state_drifts() {
    for (row, drift) in [
        // A former owner must not retain write capability past a transfer.
        ("ownership transferred", Drift::OwnershipTransferred),
        ("stored body corrupted", Drift::StoredBodyCorrupted),
        ("version row removed", Drift::VersionRowRemoved),
        // E.g. a finalize whose response was lost: no re-minting write capability.
        ("version no longer pending", Drift::VersionNoLongerPending),
    ] {
        let s = StackBuilder::new("idempotency").build().await;
        let tenant = Uuid::now_v7();
        let subject = Uuid::now_v7();
        let ctx_caller = ctx_for(tenant, subject);
        let key = "idem-drift-1";
        let auto_bind = matches!(drift, Drift::VersionNoLongerPending);

        let ticket = create_keyed(&s, &ctx_caller, subject, key, auto_bind)
            .await
            .unwrap_or_else(|e| panic!("{row}: initial create must succeed: {e:?}"));
        let conn = s.db.conn().expect("conn");
        let scope = AccessScope::allow_all();

        match drift {
            Drift::OwnershipTransferred => {
                s.svc
                    .transfer_ownership(
                        &ctx_caller,
                        ticket.file_id,
                        OwnerKind::User,
                        Uuid::now_v7(),
                    )
                    .await
                    .expect("transfer_ownership should succeed");
            }
            Drift::StoredBodyCorrupted => {
                IdemEntity::update_many()
                    .col_expr(IdemCol::ResponseBody, Expr::value("not-json"))
                    .filter(IdemCol::TenantId.eq(tenant))
                    .filter(IdemCol::OwnerId.eq(subject))
                    .filter(IdemCol::IdempotencyKey.eq(key))
                    .secure()
                    .scope_with(&scope)
                    .exec(&conn)
                    .await
                    .expect("corrupt the stored idempotency response body directly");
            }
            Drift::VersionRowRemoved => {
                VerEntity::delete_many()
                    .filter(VerCol::VersionId.eq(ticket.version_id))
                    .secure()
                    .scope_with(&scope)
                    .exec(&conn)
                    .await
                    .expect("delete the pending version row directly");
            }
            Drift::VersionNoLongerPending => {
                let finalized = file_storage::infra::storage::repo::VersionRepo::new()
                    .finalize(
                        &conn,
                        &scope,
                        ticket.file_id,
                        ticket.version_id,
                        1,
                        vec![0u8; 32],
                        "whole-sha256",
                        None,
                        None,
                    )
                    .await
                    .expect("finalize the pending version directly");
                assert!(finalized);
            }
        }

        let replay = create_keyed(&s, &ctx_caller, subject, key, auto_bind).await;
        match (drift, replay) {
            (
                Drift::OwnershipTransferred | Drift::VersionNoLongerPending,
                Err(DomainError::Conflict { .. }),
            )
            | (Drift::StoredBodyCorrupted, Err(DomainError::Database { .. })) => {}
            (
                Drift::VersionRowRemoved,
                Err(DomainError::VersionNotFound {
                    file_id,
                    version_id,
                }),
            ) => {
                assert_eq!(file_id, ticket.file_id, "{row}");
                assert_eq!(version_id, ticket.version_id, "{row}");
            }
            (_, other) => panic!("{row}: unexpected replay outcome {other:?}"),
        }
    }
}

/// A replay with a different bind mode conflicts exactly like one with a different request body.
#[tokio::test]
async fn idempotency_replay_rejects_changed_bind_mode() {
    let s = StackBuilder::new("idempotency").build().await;
    let subject = Uuid::now_v7();
    let ctx_caller = ctx_for(Uuid::now_v7(), subject);
    let key = "idem-mismatch-1";

    create_keyed(&s, &ctx_caller, subject, key, false)
        .await
        .expect("initial create with bind=false");

    let mut renamed = common::new_file_for(subject, "upload.bin", "application/octet-stream");
    renamed.name = "different-name.bin".to_owned();
    let hash_mismatch_err = s
        .svc
        .create_file(&ctx_caller, renamed, Some(key.to_owned()), false)
        .await
        .expect_err("a different request body must conflict");
    let DomainError::Conflict {
        message: hash_mismatch_message,
    } = hash_mismatch_err
    else {
        panic!("expected Conflict, got {hash_mismatch_err:?}");
    };

    let bind_replay_err = create_keyed(&s, &ctx_caller, subject, key, true)
        .await
        .expect_err("a replay with a different bind mode must conflict");
    let DomainError::Conflict {
        message: bind_mismatch_message,
    } = bind_replay_err
    else {
        panic!("expected Conflict, got {bind_replay_err:?}");
    };

    assert_eq!(
        bind_mismatch_message, hash_mismatch_message,
        "a bind-mode mismatch must produce the exact same conflict as a request_hash mismatch"
    );
}

#[tokio::test]
async fn idempotency_replay_same_bind_reissues_original_mode() {
    let s = StackBuilder::new("idempotency").build().await;
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(Uuid::now_v7());

    let first = create_keyed(&s, &ctx_caller, subject, "idem-bind-same-1", true)
        .await
        .expect("initial create with bind=true");
    let replay = create_keyed(&s, &ctx_caller, subject, "idem-bind-same-1", true)
        .await
        .expect("replay with the same bind mode must succeed");

    assert_eq!(replay.file_id, first.file_id);
    assert_eq!(replay.version_id, first.version_id);

    let claims = s
        .svc
        .verifier()
        .verify(
            token_of(&replay.upload_url),
            time::OffsetDateTime::now_utc(),
        )
        .expect("replayed token must verify");
    assert!(
        claims.bind_on_finalize,
        "replayed token must keep the originally-recorded auto_bind=true"
    );
}

#[tokio::test]
async fn idempotency_replay_after_delete_mints_an_independent_file() {
    let s = StackBuilder::new("idempotency").build().await;
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(Uuid::now_v7());

    let first = create_keyed(&s, &ctx_caller, subject, "idem-delete-1", false)
        .await
        .expect("initial create should succeed");
    s.svc
        .delete_file(&ctx_caller, first.file_id, Some("*"))
        .await
        .expect("delete_file should succeed");

    let replay = create_keyed(&s, &ctx_caller, subject, "idem-delete-1", false)
        .await
        .expect("replay after delete must still succeed as a fresh create");
    assert_ne!(
        replay.file_id, first.file_id,
        "must never mint a token against the deleted file"
    );
}

fn max_size_claim(url: &str, verifier: &file_storage::infra::signed_url::Verifier) -> Option<u64> {
    let now = time::OffsetDateTime::now_utc();
    verifier
        .verify(common::token_of(url), now)
        .expect("token must verify")
        .upload
        .max_size
}

#[tokio::test]
async fn idempotency_replay_reflects_tightened_size_policy() {
    let s = StackBuilder::new("mp-test").build().await;
    let Stack {
        issuer, psvc, svc, ..
    } = s;
    let verifier = issuer.verifier();

    let ctx = ctx(Uuid::now_v7());

    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(1024 * 1024),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let nf = new_file();
    let key = "size-policy-replay-key".to_owned();
    let original = svc
        .create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    assert_eq!(
        max_size_claim(&original.upload_url, &verifier),
        Some(1024 * 1024),
        "the original ticket's token must carry the permissive policy's max_size"
    );

    // Tighten the policy to 10 bytes, then replay with the same key.
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(10),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let replayed = svc.create_file(&ctx, nf, Some(key), false).await.unwrap();
    assert_eq!(replayed.file_id, original.file_id);
    assert_eq!(replayed.version_id, original.version_id);

    assert_eq!(
        max_size_claim(&replayed.upload_url, &verifier),
        Some(10),
        "a replay must re-mint the upload URL against the CURRENT (tightened) policy's \
         max_size, not silently replay the original ticket's now-stale, larger constraint"
    );
}

/// A replay of the same key with a different name or custom metadata is a `Conflict` and creates no
/// second file.
#[tokio::test]
async fn idempotency_replay_with_diverging_name_or_metadata_returns_conflict() {
    let tag = |value: &str| {
        vec![CustomMetadataEntry {
            key: "tag".to_owned(),
            value: value.to_owned(),
        }]
    };
    let mut base = new_file();
    base.name = "original.bin".to_owned();
    base.custom_metadata = tag("a");
    let mut other_name = base.clone();
    other_name.name = "different.bin".to_owned();
    let mut other_metadata = base.clone();
    other_metadata.custom_metadata = tag("b");

    for (row, diverging) in [("name", other_name), ("metadata", other_metadata)] {
        let Stack { svc, dsn, .. } = StackBuilder::new("mp-test").build().await;
        let ctx = ctx(Uuid::now_v7());
        let key = format!("diverging-{row}-key");

        svc.create_file(&ctx, base.clone(), Some(key.clone()), false)
            .await
            .unwrap();
        let err = svc
            .create_file(&ctx, diverging, Some(key), false)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::Conflict { .. }),
            "{row}: expected Conflict on a diverging {row}, got {err:?}"
        );
        assert_eq!(
            common::raw_count(&dsn, "files").await,
            1,
            "{row}: a rejected replay must not create a second file"
        );
    }
}

#[tokio::test]
async fn idempotency_different_owner_different_file() {
    let Stack { svc, .. } = StackBuilder::new("mp-test").build().await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let key = "shared-key".to_owned();

    let mut nf_a = new_file();
    nf_a.owner_id = Uuid::now_v7();
    let mut nf_b = new_file();
    nf_b.owner_id = Uuid::now_v7();

    let t_a = svc
        .create_file(&ctx, nf_a, Some(key.clone()), false)
        .await
        .unwrap();
    let t_b = svc.create_file(&ctx, nf_b, Some(key), false).await.unwrap();

    assert_ne!(
        t_a.file_id, t_b.file_id,
        "different owners must get distinct files even with the same key"
    );
}

#[tokio::test]
async fn idempotency_expiry_creates_new_file() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;
    use toolkit_security::AccessScope;

    use file_storage::infra::storage::entity::idempotency_key::{Column, Entity};

    let Stack { db, svc, .. } = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let nf = new_file();

    let key = "expiry-key".to_owned();
    let t1 = svc
        .create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    // Age the stored record past its expiry instead of waiting for the TTL.
    let conn = db.conn().expect("conn");
    let expired = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    Entity::update_many()
        .col_expr(Column::ExpiresAt, Expr::value(expired))
        .filter(Column::IdempotencyKey.eq(key.as_str()))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("expire the idempotency record");

    let t2 = svc.create_file(&ctx, nf, Some(key), false).await.unwrap();

    assert_ne!(
        t1.file_id, t2.file_id,
        "after expiry, the same key must create a new file"
    );
}
