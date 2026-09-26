#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use github_mirror::domain::repo::{SyncWatermarkRecord, SyncWatermarkRepository};
use github_mirror::domain::sync::SweepWatermark;
use github_mirror::domain::sync::task::Family;
use github_mirror::infra::storage::sea_orm_repo::SeaOrmSyncWatermarkRepository;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

const REPO: i64 = 42;
const FAMILY: &str = "issues";

struct Fixture {
    store: Arc<SeaOrmSyncWatermarkRepository>,
    sweep: SweepWatermark,
    scope: AccessScope,
    tenant: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        let db = common::inmem_db().await;
        let store = Arc::new(SeaOrmSyncWatermarkRepository::new(Arc::new(DBProvider::<
            DbError,
        >::new(
            db
        ))));
        let tenant = Uuid::new_v4();
        Self {
            store: Arc::clone(&store),
            sweep: SweepWatermark::new(store as Arc<dyn SyncWatermarkRepository>),
            scope: AccessScope::for_tenant(tenant),
            tenant,
        }
    }

    async fn store_row(&self, last_seen: Option<&str>, etag: Option<&str>) {
        self.store
            .upsert(
                &self.scope,
                self.tenant,
                SyncWatermarkRecord {
                    repo_id: REPO,
                    family: FAMILY.to_owned(),
                    last_seen_updated_at: last_seen.map(ToOwned::to_owned),
                    page1_etag: etag.map(ToOwned::to_owned),
                    sweep_in_progress: false,
                    candidate_high_water: None,
                },
            )
            .await
            .unwrap();
    }

    async fn row(&self) -> SyncWatermarkRecord {
        self.store
            .find(&self.scope, REPO, FAMILY)
            .await
            .unwrap()
            .expect("the watermark row must exist")
    }

    async fn stage(&self, candidate: Option<&str>) {
        self.sweep
            .stage(
                &self.scope,
                self.tenant,
                REPO,
                Family::Issues,
                candidate.map(at),
            )
            .await
            .unwrap();
    }

    async fn promote(&self, etag: Option<&str>) {
        self.sweep
            .promote(
                &self.scope,
                self.tenant,
                REPO,
                Family::Issues,
                etag.map(ToOwned::to_owned),
            )
            .await
            .unwrap();
    }
}

fn at(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .expect("test timestamps must parse")
        .with_timezone(&Utc)
}

#[tokio::test]
async fn a_first_sweep_starts_unbounded_and_without_a_validator() {
    let f = Fixture::new().await;

    let start = f
        .sweep
        .start_sweep(&f.scope, REPO, Family::Issues, false)
        .await
        .unwrap();

    assert_eq!(start.updated_after, None);
    assert_eq!(start.page1_etag, None);
}

#[tokio::test]
async fn a_later_sweep_starts_five_minutes_below_the_watermark_unless_forced() {
    let f = Fixture::new().await;
    f.store_row(Some("2026-09-01T10:00:00Z"), Some("W/\"one\""))
        .await;

    let start = f
        .sweep
        .start_sweep(&f.scope, REPO, Family::Issues, false)
        .await
        .unwrap();
    assert_eq!(start.updated_after, Some(at("2026-09-01T09:55:00Z")));
    assert_eq!(start.page1_etag.as_deref(), Some("W/\"one\""));

    let forced = f
        .sweep
        .start_sweep(&f.scope, REPO, Family::Issues, true)
        .await
        .unwrap();
    assert_eq!(forced.updated_after, None, "force ignores the watermark");
    assert_eq!(forced.page1_etag, None, "force ignores the validator too");
}

#[tokio::test]
async fn staging_parks_the_candidate_and_leaves_the_watermark_alone() {
    let f = Fixture::new().await;
    f.store_row(Some("2026-09-01T10:00:00Z"), Some("W/\"one\""))
        .await;

    f.stage(Some("2026-09-01T11:00:00Z")).await;

    let row = f.row().await;
    assert_eq!(
        row.candidate_high_water.as_deref(),
        Some("2026-09-01T11:00:00Z")
    );
    assert_eq!(
        row.last_seen_updated_at.as_deref(),
        Some("2026-09-01T10:00:00Z"),
        "the watermark only moves when the family finishes"
    );
    assert_eq!(row.page1_etag.as_deref(), Some("W/\"one\""));
    assert!(row.sweep_in_progress);
}

#[tokio::test]
async fn promoting_moves_the_watermark_forward_and_closes_the_sweep() {
    let f = Fixture::new().await;
    f.store_row(Some("2026-09-01T10:00:00Z"), Some("W/\"one\""))
        .await;

    f.stage(Some("2026-09-01T11:00:00Z")).await;
    f.promote(Some("W/\"two\"")).await;

    let row = f.row().await;
    assert_eq!(
        row.last_seen_updated_at.as_deref(),
        Some("2026-09-01T11:00:00Z")
    );
    assert_eq!(row.page1_etag.as_deref(), Some("W/\"two\""));
    assert!(!row.sweep_in_progress);
    assert_eq!(row.candidate_high_water, None);
}

#[tokio::test]
async fn an_idle_sweep_cannot_move_the_watermark_back() {
    let f = Fixture::new().await;
    f.store_row(Some("2026-09-01T10:00:00Z"), Some("W/\"one\""))
        .await;

    let start = f
        .sweep
        .start_sweep(&f.scope, REPO, Family::Issues, false)
        .await
        .unwrap();
    f.sweep
        .stage(
            &f.scope,
            f.tenant,
            REPO,
            Family::Issues,
            start.updated_after,
        )
        .await
        .unwrap();
    f.promote(None).await;

    assert_eq!(
        f.row().await.last_seen_updated_at.as_deref(),
        Some("2026-09-01T10:00:00Z"),
        "a sweep that saw nothing new stages its own lower bound, which must not be promoted"
    );
}

#[tokio::test]
async fn a_family_with_nothing_in_it_still_closes_its_row() {
    let f = Fixture::new().await;
    f.store_row(Some("2026-09-01T10:00:00Z"), Some("W/\"one\""))
        .await;

    f.stage(None).await;
    assert!(f.row().await.sweep_in_progress);

    f.promote(Some("W/\"two\"")).await;

    let row = f.row().await;
    assert!(
        !row.sweep_in_progress,
        "nothing was staged, but the sweep did finish"
    );
    assert_eq!(
        row.last_seen_updated_at.as_deref(),
        Some("2026-09-01T10:00:00Z"),
        "the watermark it already had stays"
    );
    assert_eq!(row.page1_etag.as_deref(), Some("W/\"two\""));
}

#[tokio::test]
async fn promoting_a_repository_that_never_swept_writes_nothing() {
    let f = Fixture::new().await;

    f.promote(Some("W/\"one\"")).await;

    assert!(
        f.store
            .find(&f.scope, REPO, FAMILY)
            .await
            .unwrap()
            .is_none()
    );
}
