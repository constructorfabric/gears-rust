//! Bind outcome of a single-part finalize retry (`finalize_upload_by_token` on an already
//! `available` version): `Bound` is sticky, a manual upload never reports a bind outcome, and a
//! lost CAS is re-read against the live pointer.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use bytes::Bytes;
use uuid::Uuid;

use file_storage::domain::etag::content_etag;
use file_storage::domain::multipart::BindState;
use file_storage::domain::service::FinalizeByTokenOutcome;
use file_storage::infra::content::hash;
use file_storage::infra::signed_url::Issuer;

mod common;
use common::{Stack, backend_path, ctx, new_file, put_claims};

/// One upload of `data` to `(file_id, version_id)` through the token path, as the sidecar does it.
struct Upload {
    file_id: Uuid,
    version_id: Uuid,
    auto_bind: bool,
    data: &'static [u8],
}

impl Upload {
    async fn finalize(&self, s: &Stack) -> FinalizeByTokenOutcome {
        let path = backend_path(self.file_id, self.version_id);
        // A retry finds the object already stored (create-exclusive publish).
        if !s.backend.exists(&path).await.unwrap() {
            s.backend
                .put(&path, Bytes::from_static(self.data))
                .await
                .unwrap();
        }
        let claims = put_claims(self.file_id, self.version_id, path, self.auto_bind);
        let size = i64::try_from(self.data.len()).unwrap();
        s.svc
            .finalize_upload_by_token(&claims, size, hash::sha256(self.data))
            .await
            .unwrap()
    }
}

async fn stack() -> Stack {
    common::build_stack(
        "finalize-replay-bind",
        Arc::new(Issuer::generate(3600).expect("issuer")),
        None,
    )
    .await
}

fn assert_outcome(
    out: &FinalizeByTokenOutcome,
    state: BindState,
    etag: Option<&str>,
    current_etag: Option<&str>,
    row: &str,
) {
    assert_eq!(out.bind_state, Some(state), "{row}");
    assert_eq!(out.etag.as_deref(), etag, "{row}: etag");
    assert_eq!(
        out.current_etag.as_deref(),
        current_etag,
        "{row}: current_etag"
    );
}

/// A manual upload reports no bind outcome on a retry, even after the version was bound by hand.
#[tokio::test]
async fn manual_finalize_replay_never_reports_a_bind_outcome() {
    let s = stack().await;
    let ctx = ctx(Uuid::now_v7());
    let t = s
        .svc
        .create_file(&ctx, new_file("m.bin"), None, false)
        .await
        .unwrap();
    let up = Upload {
        file_id: t.file_id,
        version_id: t.version_id,
        auto_bind: false,
        data: b"manual",
    };
    up.finalize(&s).await;
    s.svc
        .bind(&ctx, t.file_id, t.version_id, None)
        .await
        .unwrap();

    let replay = up.finalize(&s).await;
    assert_outcome(
        &replay,
        BindState::Manual,
        None,
        None,
        "manual after manual bind",
    );
}

/// A bind won at finalize replays as `Bound` after the file moved on to another version.
#[tokio::test]
async fn won_bind_replays_as_bound_after_the_file_is_rebound() {
    let s = stack().await;
    let ctx = ctx(Uuid::now_v7());
    let t = s
        .svc
        .create_file(&ctx, new_file("w.bin"), None, true)
        .await
        .unwrap();
    let up = Upload {
        file_id: t.file_id,
        version_id: t.version_id,
        auto_bind: true,
        data: b"winner",
    };
    let own_etag = content_etag(t.file_id, t.version_id);
    assert_outcome(
        &up.finalize(&s).await,
        BindState::Bound,
        Some(&own_etag),
        None,
        "first",
    );

    let t2 = s.svc.presign_version(&ctx, t.file_id).await.unwrap();
    let other = Upload {
        file_id: t.file_id,
        version_id: t2.version_id,
        auto_bind: false,
        data: b"other",
    };
    other.finalize(&s).await;
    s.svc
        .bind(&ctx, t.file_id, t2.version_id, Some(&own_etag))
        .await
        .unwrap();

    let replay = up.finalize(&s).await;
    assert_outcome(
        &replay,
        BindState::Bound,
        Some(&own_etag),
        None,
        "replay after rebind",
    );
}

/// A lost CAS replays against the live pointer: `Conflict` with the current `ETag`, then `Bound`
/// once this very version has been bound by hand.
#[tokio::test]
async fn lost_cas_replay_reads_the_live_pointer() {
    let s = stack().await;
    let ctx = ctx(Uuid::now_v7());
    let t = s
        .svc
        .create_file(&ctx, new_file("l.bin"), None, true)
        .await
        .unwrap();
    let winner = Upload {
        file_id: t.file_id,
        version_id: t.version_id,
        auto_bind: true,
        data: b"winner",
    };
    winner.finalize(&s).await;
    let winner_etag = content_etag(t.file_id, t.version_id);

    let t2 = s.svc.presign_version(&ctx, t.file_id).await.unwrap();
    let loser = Upload {
        file_id: t.file_id,
        version_id: t2.version_id,
        auto_bind: true,
        data: b"loser!",
    };
    let first = loser.finalize(&s).await;
    assert_outcome(
        &first,
        BindState::Conflict,
        None,
        Some(&winner_etag),
        "first",
    );
    let replay = loser.finalize(&s).await;
    assert_outcome(
        &replay,
        BindState::Conflict,
        None,
        Some(&winner_etag),
        "replay",
    );

    s.svc
        .bind(&ctx, t.file_id, t2.version_id, Some(&winner_etag))
        .await
        .unwrap();
    let after_bind = loser.finalize(&s).await;
    assert_outcome(
        &after_bind,
        BindState::Bound,
        Some(&content_etag(t.file_id, t2.version_id)),
        None,
        "replay after manual bind",
    );
}
