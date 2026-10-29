//! Keyset pagination of the listings at service level: full walks in both directions (including
//! `created_at` ties), cursors that do not belong to the listing, and manifest-budget truncation.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_odata::Page;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::domain::pagination;
use file_storage::domain::policy::RetentionScope;
use file_storage::domain::service::FileService;
use file_storage_sdk::{FileVersion, OwnerFilter, OwnerKind};

mod common;
use common::{
    Stack, StackBuilder, ctx_for, insert_composite_version, seed_file_with_versions, seed_files,
    valid_rule_body,
};

const PAGE_LIMIT: u64 = 2;

/// Parse a wire token the way the `OData` extractor does before the services see it.
fn cur(token: &str) -> toolkit_odata::CursorV1 {
    toolkit_odata::CursorV1::decode(token).expect("decode cursor")
}

async fn stack() -> Stack {
    StackBuilder::new("cursor-pagination")
        .page_sizes(25, 200)
        .build()
        .await
}

/// Canonical order for every listing: `created_at DESC`, id column `DESC`.
fn expected_order(instants: &[OffsetDateTime], ids: &[Uuid]) -> Vec<Uuid> {
    let mut items: Vec<_> = instants.iter().zip(ids).collect();
    items.sort_by(|a, b| b.0.cmp(a.0).then(b.1.cmp(a.1)));
    items.into_iter().map(|(_, id)| *id).collect()
}

/// Seven items over three instants: a walk with a small `limit` crosses a `created_at` tie.
fn tied_instants() -> Vec<OffsetDateTime> {
    let base = OffsetDateTime::now_utc();
    let at = |secs| base + time::Duration::seconds(secs);
    vec![base, base, base, at(1), at(1), at(2), at(2)]
}

/// One page of a listing, reduced to what a walk needs.
struct Fetched {
    ids: Vec<Uuid>,
    next: Option<String>,
    prev: Option<String>,
}

fn fetched<T>(page: &Page<T>, id_of: impl Fn(&T) -> Uuid) -> Fetched {
    Fetched {
        ids: page.items.iter().map(id_of).collect(),
        next: page.page_info.next_cursor.clone(),
        prev: page.page_info.prev_cursor.clone(),
    }
}

/// Walks a listing forward in pages of [`PAGE_LIMIT`]: every item exactly once in `expected` order
/// with no `prev_cursor` on the first page; then back from the last page's `prev_cursor`, which
/// reproduces every earlier page.
async fn assert_walks_both_ways(
    expected: &[Uuid],
    fetch: impl AsyncFn(Option<toolkit_odata::CursorV1>) -> Fetched,
) {
    let mut pages: Vec<Vec<Uuid>> = Vec::new();
    let mut prev_cursors: Vec<Option<String>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = fetch(cursor.as_deref().map(cur)).await;
        pages.push(page.ids);
        prev_cursors.push(page.prev);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(pages.len() <= expected.len(), "walk must terminate");
    }

    let seen: Vec<Uuid> = pages.iter().flatten().copied().collect();
    assert_eq!(
        seen, expected,
        "the walk must cover every item exactly once, in order"
    );
    assert_eq!(prev_cursors[0], None, "the first page has no predecessor");
    assert!(pages.len() > 1, "the walk must span more than one page");

    let mut back_pages: Vec<Vec<Uuid>> = Vec::new();
    let mut back_cursor = prev_cursors
        .last()
        .cloned()
        .flatten()
        .expect("the last page must have a predecessor");
    loop {
        let page = fetch(Some(cur(&back_cursor))).await;
        back_pages.push(page.ids);
        match page.prev {
            Some(prev) => back_cursor = prev,
            None => break,
        }
    }
    back_pages.reverse();
    assert_eq!(
        back_pages,
        &pages[..pages.len() - 1],
        "walking prev_cursor backward from the last page must reproduce every earlier page"
    );
}

#[tokio::test]
async fn files_cursor_walk_covers_all_items_exactly_once_including_created_at_ties() {
    let Stack { db, svc, .. } = stack().await;
    let (tenant, owner) = (Uuid::now_v7(), Uuid::now_v7());
    let instants = tied_instants();
    let ids = seed_files(&db, tenant, owner, &instants).await;
    let ctx = ctx_for(tenant, owner);
    let filter = OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id: owner,
    };

    assert_walks_both_ways(&expected_order(&instants, &ids), async |cursor| {
        let page = svc
            .list_files(&ctx, filter, Some(PAGE_LIMIT), cursor)
            .await
            .expect("list_files page");
        fetched(&page, |f| f.file_id)
    })
    .await;
}

#[tokio::test]
async fn versions_cursor_walk_covers_all_items_exactly_once_including_created_at_ties() {
    let Stack { db, svc, .. } = stack().await;
    let (tenant, owner) = (Uuid::now_v7(), Uuid::now_v7());
    let instants = tied_instants();
    let (file_id, ids) = seed_file_with_versions(&db, tenant, owner, &instants).await;
    let ctx = ctx_for(tenant, owner);

    assert_walks_both_ways(&expected_order(&instants, &ids), async |cursor| {
        let page = svc
            .list_versions(&ctx, file_id, Some(PAGE_LIMIT), cursor)
            .await
            .expect("list_versions page");
        fetched(&page, |v| v.version_id)
    })
    .await;
}

#[tokio::test]
async fn retention_rules_cursor_walk_covers_every_rule_in_tenant() {
    let Stack { psvc, .. } = stack().await;
    let ctx = ctx_for(Uuid::now_v7(), Uuid::now_v7());
    let mut created = Vec::new();
    for _ in 0..5 {
        let rule = psvc
            .create_retention_rule(&ctx, RetentionScope::Tenant, None, valid_rule_body())
            .await
            .expect("create rule");
        created.push(rule.rule_id);
    }
    // Rules are created one after another: newest first.
    created.reverse();

    assert_walks_both_ways(&created, async |cursor| {
        let page = psvc
            .list_retention_rules(&ctx, Some(PAGE_LIMIT), cursor)
            .await
            .expect("list_retention_rules page");
        fetched(&page, |r| r.rule_id)
    })
    .await;
}

/// A cursor issued for one owner / file is rejected when presented for another.
#[tokio::test]
async fn cursor_issued_for_another_owner_or_file_is_rejected() {
    let Stack { db, svc, .. } = stack().await;
    let tenant = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let (owner_a, owner_b) = (Uuid::now_v7(), Uuid::now_v7());
    seed_files(&db, tenant, owner_a, &[now, now]).await;
    seed_files(&db, tenant, owner_b, &[now]).await;
    let (file_a, _) = seed_file_with_versions(&db, tenant, owner_a, &[now, now]).await;
    let (file_b, _) = seed_file_with_versions(&db, tenant, owner_a, &[now]).await;
    let ctx_a = ctx_for(tenant, owner_a);
    let filter = |owner_id| OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id,
    };

    let files_cursor = svc
        .list_files(&ctx_a, filter(owner_a), Some(1), None)
        .await
        .expect("first page of files")
        .page_info
        .next_cursor
        .expect("more files remain");
    let err = svc
        .list_files(&ctx_a, filter(owner_b), Some(1), Some(cur(&files_cursor)))
        .await
        .expect_err("a cursor issued for a different owner must be rejected");
    assert!(matches!(err, DomainError::Pagination(_)), "files: {err:?}");

    let versions_cursor = svc
        .list_versions(&ctx_a, file_a, Some(1), None)
        .await
        .expect("first page of versions")
        .page_info
        .next_cursor
        .expect("more versions remain");
    let err = svc
        .list_versions(&ctx_a, file_b, Some(1), Some(cur(&versions_cursor)))
        .await
        .expect_err("a cursor issued for a different file must be rejected");
    assert!(
        matches!(err, DomainError::Pagination(_)),
        "versions: {err:?}"
    );
}

/// A caller who cannot see the file gets the same answer as without a cursor, even when the cursor
/// was issued for another file: access is checked before the cursor is parsed.
#[tokio::test]
async fn versions_with_manifests_checks_access_before_the_cursor() {
    let Stack { db, svc, .. } = stack().await;
    let (tenant, owner) = (Uuid::now_v7(), Uuid::now_v7());
    let now = OffsetDateTime::now_utc();
    let (file_a, _) = seed_file_with_versions(&db, tenant, owner, &[now, now]).await;
    let (file_b, _) = seed_file_with_versions(&db, tenant, owner, &[now]).await;

    let foreign_cursor = svc
        .list_versions(&ctx_for(tenant, owner), file_a, Some(1), None)
        .await
        .expect("first page")
        .page_info
        .next_cursor
        .expect("more pages remain");

    let outsider = ctx_for(Uuid::now_v7(), Uuid::now_v7());
    let without_cursor = svc
        .list_versions_with_manifests(&outsider, file_b, Some(1), None)
        .await
        .expect_err("an outsider must not see the file");
    let with_cursor = svc
        .list_versions_with_manifests(&outsider, file_b, Some(1), Some(cur(&foreign_cursor)))
        .await
        .expect_err("an outsider must not see the file");
    assert!(
        !matches!(with_cursor, DomainError::Pagination(_)),
        "access must be checked before the cursor, got {with_cursor:?}"
    );
    assert_eq!(
        std::mem::discriminant(&with_cursor),
        std::mem::discriminant(&without_cursor),
        "with a cursor: {with_cursor:?}, without: {without_cursor:?}"
    );
}

/// A cursor built under a different order must be rejected, in either direction.
#[tokio::test]
async fn retention_rules_cursor_with_wrong_order_field_is_rejected() {
    let Stack { psvc, .. } = stack().await;
    let ctx = ctx_for(Uuid::now_v7(), Uuid::now_v7());
    for direction in [
        pagination::Direction::Forward,
        pagination::Direction::Backward,
    ] {
        let bogus = pagination::encode(
            OffsetDateTime::now_utc(),
            Uuid::now_v7(),
            pagination::FILES_ID_FIELD, // wrong id field for retention rules
            None,
            direction,
        )
        .expect("encode");
        let err = psvc
            .list_retention_rules(&ctx, Some(PAGE_LIMIT), Some(cur(&bogus)))
            .await
            .expect_err("a cursor built for a different listing's order must be rejected");
        assert!(
            matches!(err, DomainError::Pagination(_)),
            "{direction:?}: got {err:?}"
        );
    }
}

/// Three composite versions whose 3 MiB manifests each exceed half of the 4 MiB page budget, so
/// every `list_versions_with_manifests` page holds exactly one row. Returns the service, caller
/// context, file id and the version ids newest first: `[v2, v1, v0]`.
async fn seed_budget_busting_versions() -> (Arc<FileService>, SecurityContext, Uuid, [Uuid; 3]) {
    let Stack { db, svc, .. } = stack().await;
    let (tenant, owner) = (Uuid::now_v7(), Uuid::now_v7());
    let base = OffsetDateTime::now_utc();
    let (file_id, _) = seed_file_with_versions(&db, tenant, owner, &[]).await;

    let manifest_text = "x".repeat(3 * 1024 * 1024);
    let mut version_ids = Vec::new();
    for i in 0..3i64 {
        let created_at = base + time::Duration::seconds(i);
        version_ids.push(insert_composite_version(&db, file_id, created_at, &manifest_text).await);
    }
    (
        svc,
        ctx_for(tenant, owner),
        file_id,
        [version_ids[2], version_ids[1], version_ids[0]],
    )
}

fn version_ids_of(page: &Page<(FileVersion, Option<String>)>) -> Vec<Uuid> {
    page.items.iter().map(|(v, _)| v.version_id).collect()
}

/// A forward page cut short by the manifest budget keeps the leading rows (adjacent to the
/// cursor) and rebuilds `next_cursor` from the last kept row.
#[tokio::test]
async fn versions_manifest_budget_truncation_forces_next_cursor_with_no_gap_or_duplicate() {
    let (svc, ctx, file_id, [v2, v1, v0]) = seed_budget_busting_versions().await;

    let page1 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), None)
        .await
        .expect("page 1");
    assert_eq!(
        version_ids_of(&page1),
        vec![v2],
        "the manifest budget must cut the page short after the first version"
    );
    assert!(page1.page_info.prev_cursor.is_none());
    let cursor = page1
        .page_info
        .next_cursor
        .clone()
        .expect("a budget-truncated page must still carry a next_cursor");

    let page2 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(cur(&cursor)))
        .await
        .expect("page 2");
    assert_eq!(
        version_ids_of(&page2),
        vec![v1],
        "page 2 must resume exactly at v1: no gap, no repeat of v2"
    );
    let cursor2 = page2
        .page_info
        .next_cursor
        .clone()
        .expect("page 2 is budget-truncated too, so it must also carry a next_cursor");

    let page3 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(cur(&cursor2)))
        .await
        .expect("page 3");
    assert_eq!(
        version_ids_of(&page3),
        vec![v0],
        "page 3 must resume exactly at v0: no gap, no repeat of v1"
    );
    assert!(page3.page_info.next_cursor.is_none(), "the last page");
}

/// A backward page cut short by the manifest budget keeps the TRAILING rows (the ones adjacent to
/// its cursor in canonical newest-first order) and rebuilds `prev_cursor` from the first kept row;
/// `next_cursor` stays valid. Walking back from the last page then loses and repeats nothing.
#[tokio::test]
async fn versions_manifest_budget_truncation_on_a_backward_page_keeps_rows_adjacent_to_the_cursor()
{
    let (svc, ctx, file_id, [v2, v1, v0]) = seed_budget_busting_versions().await;

    // Walk forward to the last page to obtain a genuine `prev_cursor` positioned at v0.
    let mut cursor = None;
    let mut last_page = None;
    for _ in 0..3 {
        let page = svc
            .list_versions_with_manifests(&ctx, file_id, Some(10), cursor.as_deref().map(cur))
            .await
            .expect("forward walk");
        cursor = page.page_info.next_cursor.clone();
        last_page = Some(page);
    }
    let last_page = last_page.unwrap();
    assert_eq!(version_ids_of(&last_page), vec![v0]);
    let prev_from_v0 = last_page
        .page_info
        .prev_cursor
        .clone()
        .expect("the last page has an earlier page");

    // Backward from v0: v2 and v1 are both newer, but the budget fits only one manifest. The row
    // adjacent to the cursor (v1) must be kept, not the far one (v2).
    let back1 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(cur(&prev_from_v0)))
        .await
        .expect("backward page 1");
    assert_eq!(
        version_ids_of(&back1),
        vec![v1],
        "a truncated backward page must keep the row adjacent to its cursor"
    );
    assert!(
        back1.items[0].1.is_some(),
        "the kept composite version still carries its manifest"
    );
    let prev_from_v1 = back1
        .page_info
        .prev_cursor
        .clone()
        .expect("truncation must leave a prev_cursor for the dropped newer row");
    let next_from_v1 = back1
        .page_info
        .next_cursor
        .clone()
        .expect("the opposite-direction cursor must stay valid");

    // The rebuilt `prev_cursor` re-serves exactly the dropped row.
    let back2 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(cur(&prev_from_v1)))
        .await
        .expect("backward page 2");
    assert_eq!(
        version_ids_of(&back2),
        vec![v2],
        "backward page 2 must resume exactly at v2: no gap, no repeat of v1"
    );
    assert!(
        back2.page_info.prev_cursor.is_none(),
        "v2 is the newest version: nothing earlier"
    );

    // The untouched `next_cursor` of the truncated backward page leads forward to v0 again.
    let forward = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(cur(&next_from_v1)))
        .await
        .expect("forward from the backward page");
    assert_eq!(version_ids_of(&forward), vec![v0]);
}
