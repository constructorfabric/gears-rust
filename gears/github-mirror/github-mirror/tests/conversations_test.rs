#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use github_mirror::domain::repo::{
    CommentRecord, CommentRepository, ConversationRepository, ReviewCommentRecord,
    ReviewCommentRepository, ReviewThreadRecord, ReviewThreadRepository,
};
use github_mirror::domain::sync::group_conversations;
use github_mirror::infra::storage::sea_orm_repo::{
    SeaOrmCommentRepository, SeaOrmConversationRepository, SeaOrmReviewCommentRepository,
    SeaOrmReviewThreadRepository,
};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

const REPO: i64 = 1;

type Row = (String, i64, i64, i64, Option<bool>);

fn review_comment(id: i64, reply_to: Option<i64>, at: &str) -> ReviewCommentRecord {
    ReviewCommentRecord {
        id,
        repo_id: REPO,
        pull_number: 10,
        author_login: None,
        body: Some(format!("inline body {id}")),
        path: None,
        diff_hunk: None,
        in_reply_to_id: reply_to,
        commit_id: None,
        created_at: at.to_owned(),
        updated_at: at.to_owned(),
        html_url: None,
        position: None,
        original_position: None,
        line: None,
        original_line: None,
        start_line: None,
        original_start_line: None,
        side: None,
        start_side: None,
        subject_type: None,
        pull_request_review_id: None,
        snippet_before: None,
        snippet_after: None,
    }
}

fn comment(id: i64, body: &str, at: &str) -> CommentRecord {
    CommentRecord {
        id,
        repo_id: REPO,
        issue_number: 20,
        author_login: None,
        body: Some(body.to_owned()),
        created_at: at.to_owned(),
        updated_at: at.to_owned(),
        html_url: None,
    }
}

#[tokio::test]
async fn groups_inline_chains_and_quoted_toplevel_comments() {
    let db = Arc::new(DBProvider::<DbError>::new(common::inmem_db().await));
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let review_comments = SeaOrmReviewCommentRepository::new(Arc::clone(&db));
    let comments = SeaOrmCommentRepository::new(Arc::clone(&db));
    let threads = SeaOrmReviewThreadRepository::new(Arc::clone(&db));
    let conversations = SeaOrmConversationRepository::new(Arc::clone(&db));

    for record in [
        review_comment(1, None, "2026-01-01T10:00:00Z"),
        review_comment(2, Some(1), "2026-01-01T10:05:00Z"),
        review_comment(3, None, "2026-01-01T11:00:00Z"),
    ] {
        review_comments
            .upsert(&scope, tenant, record)
            .await
            .unwrap();
    }
    threads
        .upsert(
            &scope,
            tenant,
            ReviewThreadRecord {
                id: "T1".to_owned(),
                repo_id: REPO,
                pull_number: 10,
                is_resolved: true,
                is_outdated: false,
                path: None,
                line: None,
                resolved_by: None,
                comments_count: 2,
            },
        )
        .await
        .unwrap();
    for record in [
        comment(
            100,
            "Let's discuss the new caching strategy in detail",
            "2026-01-02T10:00:00Z",
        ),
        comment(
            101,
            "> the new caching strategy in detail\nAgreed, go on",
            "2026-01-02T10:10:00Z",
        ),
        comment(
            102,
            "Completely unrelated CI flakiness report",
            "2026-01-02T10:20:00Z",
        ),
    ] {
        comments.upsert(&scope, tenant, record).await.unwrap();
    }

    let stats = group_conversations(&conversations, &scope, tenant, REPO, None)
        .await
        .unwrap();
    assert_eq!((stats.inline, stats.toplevel), (2, 2));

    let all = conversations.list(&scope, REPO, None).await.unwrap();
    let summary: Vec<Row> = all
        .iter()
        .map(|c| {
            (
                c.conv_type.clone(),
                c.parent_number,
                c.root_comment_id,
                c.comment_count,
                c.is_resolved,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("inline".to_owned(), 10, 1, 2, Some(true)),
            ("inline".to_owned(), 10, 3, 1, None),
            ("toplevel".to_owned(), 20, 100, 2, None),
            ("toplevel".to_owned(), 20, 102, 1, None),
        ]
    );
    assert_eq!(all[2].created_at.as_deref(), Some("2026-01-02T10:00:00Z"));

    let members: Vec<i64> = conversations
        .comments_in(&scope, REPO, 100)
        .await
        .unwrap()
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(members, vec![100, 101]);
    let members: Vec<i64> = conversations
        .review_comments_in(&scope, REPO, 1)
        .await
        .unwrap()
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(members, vec![1, 2]);

    let only_20 = conversations.list(&scope, REPO, Some(20)).await.unwrap();
    assert_eq!(only_20.len(), 2);

    let nothing_changed = group_conversations(
        &conversations,
        &scope,
        tenant,
        REPO,
        Some(Utc::now() + Duration::hours(1)),
    )
    .await
    .unwrap();
    assert_eq!((nothing_changed.inline, nothing_changed.toplevel), (0, 0));

    let incremental = group_conversations(
        &conversations,
        &scope,
        tenant,
        REPO,
        Some(Utc::now() - Duration::hours(1)),
    )
    .await
    .unwrap();
    assert_eq!((incremental.inline, incremental.toplevel), (2, 2));
    assert_eq!(conversations.list(&scope, REPO, None).await.unwrap(), all);

    let stranger = Uuid::new_v4();
    assert!(
        conversations
            .list(&AccessScope::for_tenant(stranger), REPO, None)
            .await
            .unwrap()
            .is_empty()
    );
}
