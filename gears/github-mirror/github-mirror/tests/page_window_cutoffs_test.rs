#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use github_mirror::domain::repo::{
    CommentRecord, CommentRepository, LabelRecord, LabelRepository, PageWindow,
};
use github_mirror::infra::storage::sea_orm_repo::{SeaOrmCommentRepository, SeaOrmLabelRepository};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

const REPO: i64 = 1;
const ISSUE: i64 = 7;

fn comment(id: i64, updated_at: &str) -> CommentRecord {
    CommentRecord {
        id,
        repo_id: REPO,
        issue_number: ISSUE,
        author_login: None,
        body: None,
        created_at: updated_at.to_owned(),
        updated_at: updated_at.to_owned(),
        html_url: None,
    }
}

fn label(id: i64) -> LabelRecord {
    LabelRecord {
        id,
        repo_id: REPO,
        name: format!("label-{id}"),
        color: "ffffff".to_owned(),
        is_default: false,
        description: None,
    }
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

async fn store() -> (Arc<DBProvider<DbError>>, Uuid, AccessScope) {
    let db = Arc::new(DBProvider::<DbError>::new(common::inmem_db().await));
    let tenant = Uuid::new_v4();
    (db, tenant, AccessScope::for_tenant(tenant))
}

#[tokio::test]
async fn updated_since_keeps_rows_github_changed_at_or_after_the_cutoff() {
    let (db, tenant, scope) = store().await;
    let comments = SeaOrmCommentRepository::new(db);
    for record in [
        comment(1, "2026-01-01T00:00:00Z"),
        comment(2, "2026-02-01T00:00:00Z"),
        comment(3, "2026-03-01T00:00:00Z"),
    ] {
        comments.upsert(&scope, tenant, record).await.unwrap();
    }
    let window = PageWindow::bounded(10, 0)
        .unwrap()
        .with_updated_since(Some(at("2026-02-01T00:00:00Z")));

    let ids: Vec<i64> = comments
        .list_by_issue(&scope, REPO, ISSUE, window)
        .await
        .unwrap()
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(ids, vec![2, 3]);
}

#[tokio::test]
async fn extracted_since_keeps_rows_a_sync_wrote_at_or_after_the_cutoff() {
    let (db, tenant, scope) = store().await;
    let comments = SeaOrmCommentRepository::new(db);
    comments
        .upsert(&scope, tenant, comment(1, "2026-01-01T00:00:00Z"))
        .await
        .unwrap();
    let all = PageWindow::bounded(10, 0).unwrap();

    let later = all.with_extracted_since(Some(Utc::now() + Duration::hours(1)));
    assert!(
        comments
            .list_by_issue(&scope, REPO, ISSUE, later)
            .await
            .unwrap()
            .is_empty()
    );
    let earlier = all.with_extracted_since(Some(Utc::now() - Duration::hours(1)));
    assert_eq!(
        comments
            .list_by_issue(&scope, REPO, ISSUE, earlier)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn an_entity_without_a_github_stamp_ignores_updated_since_but_not_extracted_since() {
    let (db, tenant, scope) = store().await;
    let labels = SeaOrmLabelRepository::new(db);
    labels.upsert(&scope, tenant, label(1)).await.unwrap();
    labels.upsert(&scope, tenant, label(2)).await.unwrap();
    let all = PageWindow::bounded(10, 0).unwrap();

    let since_tomorrow = all.with_updated_since(Some(Utc::now() + Duration::days(1)));
    assert_eq!(
        labels
            .list_by_repo(&scope, REPO, since_tomorrow)
            .await
            .unwrap()
            .len(),
        2
    );
    let written_tomorrow = all.with_extracted_since(Some(Utc::now() + Duration::days(1)));
    assert!(
        labels
            .list_by_repo(&scope, REPO, written_tomorrow)
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_window_without_cutoffs_reports_none_for_both() {
    let window = PageWindow::bounded(5, 0).unwrap();
    assert_eq!(window.extracted_since(), None);
    assert_eq!(window.updated_since(), None);
    let cutoff = at("2026-05-01T00:00:00Z");
    let narrowed = window
        .with_updated_since(Some(cutoff))
        .with_extracted_since(Some(cutoff));
    assert_eq!(narrowed.updated_since(), Some(cutoff));
    assert_eq!(narrowed.extracted_since(), Some(cutoff));
    assert_eq!(narrowed.limit(), 5);
}
