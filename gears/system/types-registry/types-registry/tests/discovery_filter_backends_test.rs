//! Discovery filters (T22c) in `EntityRepo::list_page`, on every backend.
//!
//! `pattern`, `depth`, `kind` and `lifecycle` are all SQL predicates applied before
//! `LIMIT`, so a sparse match set still fills every page but the last.
//! `discovery_pattern_backends_test` covers pattern semantics. `SQLite` runs
//! unconditionally; `PostgreSQL` and `MySQL` need Docker:
//!
//! ```text
//! cargo test -p cf-gears-types-registry --features integration --test discovery_filter_backends_test
//! ```

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::num::NonZeroU8;
use std::sync::Arc;

use gts::GtsIdPattern;
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use uuid::Uuid;

use common::allow_all;
use types_registry::domain::enums::{EntityKind, LifecycleFilter, OwnershipScope};
use types_registry::domain::ports::{ListFilter, NewEntity, PageRequest};
use types_registry::infra::storage::repo::{EntityRepo, VersionFamilyRepo};

type Provider = Arc<DBProvider<DbError>>;

const NOW: OffsetDateTime = datetime!(2026-09-23 09:00:00 UTC);

/// Type Schemas and Instances interleaved in byte order, so a kind filter must skip
/// rows between its matches.
const ROWS: &[&str] = &[
    gts_id!("cf.core.dfb.base.v1~"),
    gts_id!("cf.core.dfb.base.v1~cf.core.dfb.first.v1"),
    gts_id!("cf.core.dfb.base.v1~cf.core.dfb.mid.v1~"),
    gts_id!("cf.core.dfb.base.v1~cf.core.dfb.mid.v1~cf.core.dfb.leaf.v1"),
    gts_id!("cf.core.dfb.base.v1~cf.core.dfb.second.v1"),
    gts_id!("cf.core.dfb.other.v1~"),
];

fn kind_of(gts_id: &str) -> EntityKind {
    if gts_id.ends_with('~') {
        EntityKind::TypeSchema
    } else {
        EntityKind::Instance
    }
}

async fn seed(db: &Provider, ids: &[&str]) {
    seed_in(db, ids, None).await;
}

/// `family: None` gives every identifier its own family, since a family holds one
/// kind and [`ROWS`] mixes them.
async fn seed_in(db: &Provider, ids: &[&str], family: Option<&str>) {
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    for id in ids {
        let (family, _) = VersionFamilyRepo::create_or_get(
            &conn,
            &scope,
            &family.map_or_else(|| format!("family:{id}"), str::to_owned),
            OwnershipScope::Global,
            None,
            NOW,
        )
        .await
        .expect("family");
        EntityRepo::insert(
            &conn,
            &scope,
            NewEntity {
                gts_uuid: Uuid::new_v5(&Uuid::NAMESPACE_URL, id.as_bytes()),
                gts_id: (*id).to_owned(),
                entity_kind: kind_of(id),
                family_id: family.id,
                ownership_scope: OwnershipScope::Global,
                owner_tenant_id: None,
                owning_gear: Some("types-registry".to_owned()),
                now: NOW,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("insert {id}: {e}"))
        .expect("fresh identifier");
    }
}

/// Every page of a traversal under `filter`, `limit` rows at a time. A page with
/// a continuation must be full.
async fn traverse(db: &Provider, filter: &ListFilter, limit: u32) -> Vec<String> {
    let conn = db.conn().expect("conn");
    let mut seen = Vec::new();
    let mut request = PageRequest::first(limit);
    for _ in 0..100 {
        let page = EntityRepo::list_page(&conn, &allow_all(), filter, request)
            .await
            .expect("page");
        let full = page.items.len() == limit as usize;
        assert!(page.items.len() <= limit as usize);
        seen.extend(page.items.into_iter().map(|row| row.gts_id));
        let Some(next) = page.next_after else {
            return seen;
        };
        assert!(full, "a page with a continuation is full");
        request = PageRequest::after(next, limit);
    }
    panic!("the traversal did not end");
}

fn expected(predicate: impl Fn(&str) -> bool) -> Vec<String> {
    let mut ids: Vec<String> = ROWS
        .iter()
        .filter(|id| predicate(id))
        .map(|id| (*id).to_owned())
        .collect();
    ids.sort();
    ids
}

async fn kind_is_an_sql_predicate_intersected_with_the_pattern(db: &Provider, backend: &str) {
    for kind in [EntityKind::TypeSchema, EntityKind::Instance] {
        for limit in [1, 2, 10] {
            let filter = ListFilter {
                kind: Some(kind),
                ..ListFilter::default()
            };
            assert_eq!(
                traverse(db, &filter, limit).await,
                expected(|id| kind_of(id) == kind),
                "{kind:?} at limit {limit} on {backend}",
            );
        }
        let filter = ListFilter {
            pattern: Some(
                GtsIdPattern::try_new(gts_id!("cf.core.dfb.base.v1~*")).expect("pattern"),
            ),
            kind: Some(kind),
            lifecycle: LifecycleFilter::Active,
            max_chain_depth: None,
        };
        assert_eq!(
            traverse(db, &filter, 1).await,
            expected(|id| id.starts_with(gts_id!("cf.core.dfb.base.v1~")) && kind_of(id) == kind),
            "{kind:?} with a pattern on {backend}",
        );
    }
    assert_eq!(
        traverse(db, &ListFilter::default(), 2).await,
        expected(|_| true),
        "absent kind is no restriction on {backend}",
    );
}

fn depth_of(gts_id: &str) -> usize {
    gts::GtsId::try_new(gts_id)
        .expect("identifier")
        .segments()
        .len()
}

/// `depth` is an inclusive maximum of parsed segments, with or without the other
/// filters, and composes with small pages without losing a match.
async fn depth_bounds_parsed_segments_and_composes(db: &Provider, backend: &str) {
    let base = GtsIdPattern::try_new(gts_id!("cf.core.dfb.base.v1~*")).expect("pattern");
    for depth in [1_u8, 2, 3, 255] {
        for kind in [
            None,
            Some(EntityKind::TypeSchema),
            Some(EntityKind::Instance),
        ] {
            for pattern in [None, Some(base.clone())] {
                let filter = ListFilter {
                    pattern: pattern.clone(),
                    kind,
                    lifecycle: LifecycleFilter::Active,
                    max_chain_depth: NonZeroU8::new(depth),
                };
                let want = expected(|id| {
                    depth_of(id) <= usize::from(depth)
                        && kind.is_none_or(|kind| kind_of(id) == kind)
                        && (pattern.is_none() || id.starts_with(gts_id!("cf.core.dfb.base.v1~")))
                });
                for limit in [1, 3] {
                    assert_eq!(
                        traverse(db, &filter, limit).await,
                        want,
                        "depth {depth}, {kind:?}, pattern {}, limit {limit} on {backend}",
                        pattern.is_some(),
                    );
                }
            }
        }
    }
    assert_eq!(
        depth_of(gts_id!("cf.core.dfb.base.v1~")),
        1,
        "a one-segment root has depth 1"
    );
}

/// Many non-matching rows between two matches: the first page of one row holds
/// the first match, the second the last one, and nothing follows.
async fn a_sparse_depth_filter_fills_its_pages(db: &Provider, backend: &str) {
    const GAP: usize = 300;
    let first = gts_id!("cf.core.dfs.aaa.v1~");
    let last = gts_id!("cf.core.dfs.zzz.v1~");
    let gap: Vec<String> = (0..GAP)
        .map(|i| {
            format!(
                "{}cf.core.dfs.base.v1~cf.core.dfs.n{i:05}.v1~",
                gts::GTS_ID_PREFIX
            )
        })
        .collect();
    seed(db, &[first, last]).await;
    let gap_refs: Vec<&str> = gap.iter().map(String::as_str).collect();
    seed_in(db, &gap_refs, Some("family:sparse")).await;

    let filter = ListFilter {
        pattern: Some(GtsIdPattern::try_new(gts_id!("cf.core.dfs.*")).expect("pattern")),
        kind: None,
        lifecycle: LifecycleFilter::Active,
        max_chain_depth: NonZeroU8::new(1),
    };
    let conn = db.conn().expect("conn");
    let page = EntityRepo::list_page(&conn, &allow_all(), &filter, PageRequest::first(1))
        .await
        .expect("first page");
    assert_eq!(
        page.items
            .iter()
            .map(|row| row.gts_id.as_str())
            .collect::<Vec<_>>(),
        [first],
        "on {backend}"
    );
    let page = EntityRepo::list_page(
        &conn,
        &allow_all(),
        &filter,
        PageRequest::after(page.next_after.expect("a continuation"), 1),
    )
    .await
    .expect("second page");
    assert_eq!(
        page.items
            .iter()
            .map(|row| row.gts_id.as_str())
            .collect::<Vec<_>>(),
        [last],
        "on {backend}"
    );
    assert_eq!(page.next_after, None, "nothing follows on {backend}");
    assert_eq!(
        traverse(db, &filter, 2).await,
        [first, last],
        "on {backend}"
    );
}

/// Exactly `limit` matches fit one page with no continuation; no match is an
/// empty page with none.
async fn the_last_page_has_no_continuation(db: &Provider, backend: &str) {
    let conn = db.conn().expect("conn");
    let base = GtsIdPattern::try_new(gts_id!("cf.core.dfb.base.v1~cf.core.dfb.mid.v1~*"))
        .expect("pattern");
    let filter = ListFilter {
        pattern: Some(base),
        ..ListFilter::default()
    };
    let page = EntityRepo::list_page(&conn, &allow_all(), &filter, PageRequest::first(2))
        .await
        .expect("page");
    assert_eq!(page.items.len(), 2, "mid and its leaf on {backend}");
    assert_eq!(page.next_after, None, "exactly a full page on {backend}");

    let none = ListFilter {
        pattern: Some(GtsIdPattern::try_new(gts_id!("cf.core.dfb.absent.*")).expect("pattern")),
        ..ListFilter::default()
    };
    let page = EntityRepo::list_page(&conn, &allow_all(), &none, PageRequest::first(2))
        .await
        .expect("page");
    assert!(page.items.is_empty(), "on {backend}");
    assert_eq!(page.next_after, None, "on {backend}");
}

async fn tombstone(db: &Provider, gts_id: &str) {
    let conn = db.conn().expect("conn");
    let row = EntityRepo::find_by_gts_id(&conn, &allow_all(), gts_id)
        .await
        .expect("read")
        .expect("seeded");
    EntityRepo::mark_deleted(&conn, &allow_all(), row.id, row.resource_version, NOW)
        .await
        .expect("delete")
        .expect("active row deletes");
}

/// Two tombstones around many active rows: `deleted` lists only them, `active`
/// never shows one, `all` lists both kinds once, and `all` with `depth` returns
/// the two tombstones as full pages of one.
async fn lifecycle_is_an_sql_predicate_across_sparse_pages(db: &Provider, backend: &str) {
    const GAP: usize = 300;
    let near = gts_id!("cf.core.dfl.aaa.v1~");
    let far = gts_id!("cf.core.dfl.zzz.v1~");
    let gap: Vec<String> = (0..GAP)
        .map(|i| {
            format!(
                "{}cf.core.dfl.base.v1~cf.core.dfl.n{i:05}.v1~",
                gts::GTS_ID_PREFIX
            )
        })
        .collect();
    seed(db, &[near, far]).await;
    let gap_refs: Vec<&str> = gap.iter().map(String::as_str).collect();
    seed_in(db, &gap_refs, Some("family:lifecycle")).await;
    tombstone(db, near).await;
    tombstone(db, far).await;

    let pattern = GtsIdPattern::try_new(gts_id!("cf.core.dfl.*")).expect("pattern");
    let filter = |lifecycle, max_chain_depth| ListFilter {
        pattern: Some(pattern.clone()),
        kind: None,
        lifecycle,
        max_chain_depth,
    };
    assert_eq!(
        traverse(db, &filter(LifecycleFilter::Deleted, None), 1).await,
        [near, far],
        "deleted lists only tombstones on {backend}",
    );
    assert!(
        traverse(db, &filter(LifecycleFilter::Active, NonZeroU8::new(1)), 1)
            .await
            .is_empty(),
        "active never lists a tombstone on {backend}",
    );
    let active = traverse(db, &filter(LifecycleFilter::Active, None), 100).await;
    assert_eq!(active, gap, "active is unchanged on {backend}");
    let mut all = traverse(db, &filter(LifecycleFilter::All, None), 100).await;
    all.sort();
    let mut want = gap.clone();
    want.extend([near.to_owned(), far.to_owned()]);
    want.sort();
    assert_eq!(all, want, "all is both, each once, on {backend}");
    assert_eq!(
        traverse(db, &filter(LifecycleFilter::All, NonZeroU8::new(1)), 1).await,
        [near, far],
        "no tombstone skipped on {backend}"
    );
}

async fn assert_filters(db: &Provider, backend: &str) {
    seed(db, ROWS).await;
    kind_is_an_sql_predicate_intersected_with_the_pattern(db, backend).await;
    depth_bounds_parsed_segments_and_composes(db, backend).await;
    the_last_page_has_no_continuation(db, backend).await;
    a_sparse_depth_filter_fills_its_pages(db, backend).await;
    lifecycle_is_an_sql_predicate_across_sparse_pages(db, backend).await;
}

#[tokio::test]
async fn discovery_filters_behave_on_sqlite() {
    let db = common::test_db().await;
    assert_filters(&db, "sqlite").await;
}

#[cfg(feature = "integration")]
mod containers {
    use std::time::Duration;

    use super::assert_filters;
    use super::common::provider_for;

    async fn wait_for_tcp(host: &str, port: u16, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::net::TcpStream::connect((host, port)).await.is_err() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timeout waiting for {host}:{port}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn discovery_filters_behave_on_postgres() {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;

        let container = test_containers::postgres()
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app")
            .start()
            .await
            .expect("start postgres container");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let host = container.get_host().await.expect("host").to_string();
        wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(1)).await;
        let db = provider_for(&format!("postgres://user:pass@{host}:{port}/app"), 4).await;
        assert_filters(&db, "postgres").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn discovery_filters_behave_on_mysql() {
        use testcontainers::runners::AsyncRunner;

        let container = test_containers::mysql()
            .start()
            .await
            .expect("start mysql container");
        let port = container.get_host_port_ipv4(3306).await.expect("port");
        let host = container.get_host().await.expect("host").to_string();
        wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(2)).await;
        let db = provider_for(&format!("mysql://root@{host}:{port}/test"), 4).await;
        assert_filters(&db, "mysql").await;
    }
}
