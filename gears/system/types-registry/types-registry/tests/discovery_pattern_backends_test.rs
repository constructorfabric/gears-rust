//! Differential test for discovery's SQL pattern filter: every generated pattern
//! returns exactly the stored identifiers `GtsId::matches_pattern` accepts,
//! composed with `depth`, `kind`, `lifecycle` and keyset pages.
//!
//! The corpus covers minors in early segments, every wildcard cut, bare `~*`,
//! implicit derived coverage, instance tails and UUID-tail patterns. `SQLite`
//! runs unconditionally; `PostgreSQL` and `MySQL` need Docker:
//!
//! ```text
//! cargo test -p cf-gears-types-registry --features integration --test discovery_pattern_backends_test
//! ```

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::collections::BTreeSet;
use std::num::NonZeroU8;
use std::sync::Arc;

use gts::{GTS_ID_PREFIX, GtsId, GtsIdPattern, GtsIdSegment};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use common::allow_all;
use types_registry::domain::enums::{EntityKind, LifecycleFilter, OwnershipScope};
use types_registry::domain::ports::{ListFilter, NewEntity, PageRequest};
use types_registry::infra::storage::entity::{entity, entity_gts_segment};
use types_registry::infra::storage::repo::{EntityRepo, VersionFamilyRepo};

type Provider = Arc<DBProvider<DbError>>;

const NOW: OffsetDateTime = datetime!(2026-09-25 09:00:00 UTC);
const UUID_TAIL: &str = "7a1d2f34-5678-49ab-9012-abcdef123456";

/// Roots with prefix-sharing vendors and types, every version shape, then
/// derived types and instances whose names repeat across positions.
fn corpus() -> Vec<String> {
    let p = GTS_ID_PREFIX;
    let mut ids = BTreeSet::new();
    for vendor in ["x", "x_y", "x0"] {
        for ty in ["t", "t2"] {
            for version in ["v0", "v1", "v1.0", "v1.2", "v10"] {
                ids.insert(format!("{p}{vendor}.p.n.{ty}.{version}~"));
            }
        }
    }
    for root in ["x.p.n.t.v1~", "x.p.n.t.v1.2~", "x_y.p.n.t2.v0~"] {
        for vendor in ["a", "x"] {
            for version in ["v1", "v1.1", "v2"] {
                for marker in ["~", ""] {
                    ids.insert(format!("{p}{root}{vendor}.p.n.t.{version}{marker}"));
                }
            }
        }
    }
    for parent in ["x.p.n.t.v1~a.p.n.t.v1~", "x.p.n.t.v1.2~x.p.n.t.v1.1~"] {
        for leaf in ["e.p.n.t.v0", "e.p.n.t.v1.3~", "a.p.n.t.v1~", "a.p.n.t.v1"] {
            ids.insert(format!("{p}{parent}{leaf}"));
        }
    }
    ids.into_iter().collect()
}

/// Every cut of every corpus identifier, plus minor and UUID-tail variants.
fn patterns(ids: &[String]) -> Vec<String> {
    let p = GTS_ID_PREFIX;
    let mut out = BTreeSet::from([format!("{p}*"), format!("{p}zz.*")]);
    for id in ids {
        let parsed = GtsId::try_new(id).expect("corpus id");
        let raws: Vec<&str> = parsed.segments().iter().map(GtsIdSegment::raw).collect();
        out.insert(id.clone());
        for (k, seg) in parsed.segments().iter().enumerate() {
            let head = raws[..k].concat();
            let tail = raws[k + 1..].concat();
            let (v, pk, n, t) = (
                seg.vendor(),
                seg.package(),
                seg.namespace(),
                seg.type_name(),
            );
            let name = format!("{v}.{pk}.{n}.{t}");
            let major = seg.ver_major_opt().expect("major");
            let marker = if seg.is_type() { "~" } else { "" };
            for cut in [
                format!("{v}.*"),
                format!("{v}.{pk}.*"),
                format!("{v}.{pk}.{n}.*"),
                format!("{name}.*"),
                format!("{name}.v*"),
                format!("{name}.v{major}.*"),
            ] {
                out.insert(format!("{p}{head}{cut}"));
            }
            if k > 0 {
                out.insert(format!("{p}{head}*"));
            }
            out.insert(format!("{p}{head}{name}.v{major}{marker}{tail}"));
            out.insert(format!("{p}{head}{name}.v{major}.1{marker}{tail}"));
            if seg.is_type() {
                out.insert(format!("{p}{head}{}", seg.raw()));
                out.insert(format!("{p}{head}{name}.v{major}~"));
                out.insert(format!("{p}{head}{name}.v{major}.0~*"));
                out.insert(format!("{p}{head}{}{UUID_TAIL}", seg.raw()));
            }
        }
    }
    out.into_iter()
        .filter(|pattern| GtsIdPattern::is_valid(pattern))
        .collect()
}

fn kind_of(gts_id: &str) -> EntityKind {
    if gts_id.ends_with('~') {
        EntityKind::TypeSchema
    } else {
        EntityKind::Instance
    }
}

async fn seed(db: &Provider, ids: &[String]) {
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    for id in ids {
        let (family, _) = VersionFamilyRepo::create_or_get(
            &conn,
            &scope,
            &format!("family:{id}"),
            OwnershipScope::Global,
            None,
            NOW,
        )
        .await
        .expect("family");
        EntityRepo::insert(&conn, &scope, new_entity(id, family.id))
            .await
            .unwrap_or_else(|e| panic!("insert {id}: {e}"))
            .expect("fresh identifier");
    }
}

fn new_entity(id: &str, family_id: i64) -> NewEntity {
    NewEntity {
        gts_uuid: Uuid::new_v5(&Uuid::NAMESPACE_URL, id.as_bytes()),
        gts_id: id.to_owned(),
        entity_kind: kind_of(id),
        family_id,
        ownership_scope: OwnershipScope::Global,
        owner_tenant_id: None,
        owning_gear: Some("types-registry".to_owned()),
        now: NOW,
    }
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

/// Every page under `filter`. A page with a continuation must be full, so no
/// page is ever empty unless nothing matches.
async fn traverse(db: &Provider, filter: &ListFilter, limit: u32) -> Vec<String> {
    let conn = db.conn().expect("conn");
    let mut seen = Vec::new();
    let mut request = PageRequest::first(limit);
    for _ in 0..1000 {
        let page = EntityRepo::list_page(&conn, &allow_all(), filter, request)
            .await
            .expect("page");
        let full = page.items.len() == limit as usize;
        seen.extend(page.items.into_iter().map(|row| row.gts_id));
        let Some(next) = page.next_after else {
            return seen;
        };
        assert!(full, "a page with a continuation is full");
        assert_eq!(seen.last(), Some(&next), "the cursor is the last row");
        request = PageRequest::after(next, limit);
    }
    panic!("the traversal did not end");
}

/// Stored depth and segment rows are the parsed identifier's.
async fn stored_segments_mirror_the_parse(db: &Provider, ids: &[String], backend: &str) {
    let conn = db.conn().expect("conn");
    for id in ids {
        let parsed = GtsId::try_new(id).expect("corpus id");
        let row = entity::Entity::find()
            .filter(entity::Column::GtsId.eq(id.as_str()))
            .secure()
            .scope_with(&allow_all())
            .one(&conn)
            .await
            .expect("read")
            .expect("seeded");
        assert_eq!(
            usize::try_from(row.chain_depth).expect("positive"),
            parsed.segments().len(),
            "{id} on {backend}"
        );
        let segments = entity_gts_segment::Entity::find()
            .filter(entity_gts_segment::Column::EntityId.eq(row.id))
            .order_by_asc(entity_gts_segment::Column::SegmentNo)
            .secure()
            .scope_with(&allow_all())
            .all(&conn)
            .await
            .expect("segments");
        assert_eq!(segments.len(), parsed.segments().len(), "{id} on {backend}");
        for (i, (stored, seg)) in segments.iter().zip(parsed.segments()).enumerate() {
            assert_eq!(usize::try_from(stored.segment_no).expect("positive"), i);
            assert_eq!(
                stored.segment_name,
                format!(
                    "{}.{}.{}.{}",
                    seg.vendor(),
                    seg.package(),
                    seg.namespace(),
                    seg.type_name()
                )
            );
            assert_eq!(stored.major, i64::from(seg.ver_major_opt().expect("major")));
            assert_eq!(stored.minor, seg.ver_minor().map(i64::from));
            assert_eq!(stored.is_type, seg.is_type());
        }
    }
}

async fn assert_matches_gts(db: &Provider, backend: &str) {
    let ids = corpus();
    seed(db, &ids).await;
    stored_segments_mirror_the_parse(db, &ids, backend).await;
    let deleted: BTreeSet<&String> = ids.iter().step_by(7).collect();
    for id in &deleted {
        tombstone(db, id).await;
    }
    let parsed: Vec<(&String, GtsId)> = ids
        .iter()
        .map(|id| (id, GtsId::try_new(id).expect("corpus id")))
        .collect();
    let patterns = patterns(&ids);
    assert!(patterns.len() > 300, "{} patterns", patterns.len());

    for (i, raw) in patterns.iter().enumerate() {
        let pattern = GtsIdPattern::try_new(raw).expect("pattern");
        let matching: Vec<&(&String, GtsId)> = parsed
            .iter()
            .filter(|(_, id)| id.matches_pattern(&pattern))
            .collect();
        let all = ListFilter {
            pattern: Some(pattern.clone()),
            lifecycle: LifecycleFilter::All,
            ..ListFilter::default()
        };
        let want: Vec<String> = matching.iter().map(|(id, _)| (*id).clone()).collect();
        assert_eq!(traverse(db, &all, 1000).await, want, "{raw} on {backend}");

        if i % 7 != 0 {
            continue;
        }
        for lifecycle in [LifecycleFilter::Active, LifecycleFilter::Deleted] {
            for kind in [
                None,
                Some(EntityKind::TypeSchema),
                Some(EntityKind::Instance),
            ] {
                for depth in [None, NonZeroU8::new(1), NonZeroU8::new(2)] {
                    let filter = ListFilter {
                        pattern: Some(pattern.clone()),
                        kind,
                        lifecycle,
                        max_chain_depth: depth,
                    };
                    let want: Vec<String> = matching
                        .iter()
                        .filter(|(id, parsed)| {
                            deleted.contains(id) == (lifecycle == LifecycleFilter::Deleted)
                                && kind.is_none_or(|kind| kind_of(id) == kind)
                                && depth.is_none_or(|d| parsed.segments().len() <= d.get().into())
                        })
                        .map(|(id, _)| (*id).clone())
                        .collect();
                    assert_eq!(
                        traverse(db, &filter, 2).await,
                        want,
                        "{raw}, {lifecycle:?}, {kind:?}, depth {depth:?} on {backend}"
                    );
                }
            }
        }
    }

    let uuid_tail = format!("{GTS_ID_PREFIX}x.p.n.t.v1~{UUID_TAIL}");
    let conn = db.conn().expect("conn");
    let (family, _) = VersionFamilyRepo::create_or_get(
        &conn,
        &allow_all(),
        "family:uuid-tail",
        OwnershipScope::Global,
        None,
        NOW,
    )
    .await
    .expect("family");
    assert!(
        EntityRepo::insert(&conn, &allow_all(), new_entity(&uuid_tail, family.id))
            .await
            .is_err(),
        "a UUID tail has no stored segment shape on {backend}"
    );
    assert!(
        EntityRepo::find_by_gts_id(&conn, &allow_all(), &uuid_tail)
            .await
            .expect("read")
            .is_none(),
        "the refused identifier left no row on {backend}"
    );
}

#[tokio::test]
async fn discovery_patterns_match_gts_on_sqlite() {
    let db = common::test_db().await;
    assert_matches_gts(&db, "sqlite").await;
}

#[cfg(feature = "integration")]
mod containers {
    use std::time::Duration;

    use super::assert_matches_gts;
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
    async fn discovery_patterns_match_gts_on_postgres() {
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
        assert_matches_gts(&db, "postgres").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn discovery_patterns_match_gts_on_mysql() {
        use testcontainers::runners::AsyncRunner;

        let container = test_containers::mysql()
            .start()
            .await
            .expect("start mysql container");
        let port = container.get_host_port_ipv4(3306).await.expect("port");
        let host = container.get_host().await.expect("host").to_string();
        wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(2)).await;
        let db = provider_for(&format!("mysql://root@{host}:{port}/test"), 4).await;
        assert_matches_gts(&db, "mysql").await;
    }
}
