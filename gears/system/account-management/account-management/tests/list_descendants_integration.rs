//! Real-DB integration tests for the recursive children listing on
//! in-memory `SQLite`: `TenantRepoImpl::list_descendants` (visible set,
//! `OData` `contains` / `startswith` / `eq`, hidden-status default,
//! cursor walk, `$orderby`) and `TenantRepoImpl::ancestor_chains`, plus
//! `TenantService::list_descendants` end to end over the barrier
//! topology (`common::BarrierTopology`).
//!
//! Visibility table the suite pins (caller scoped to `root`):
//!
//! | listing root | returned          | not returned |
//! |--------------|-------------------|--------------|
//! | `root`       | `x`, `xc`, `s`, `y` | `yc`, `sc`   |
//! | `x`          | `xc`, `y`         | `yc`         |
//! | `s`          | `not_found`       | —            |
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::collections::HashMap;

use account_management::domain::error::DomainError;
use account_management::domain::tenant::TenantRepo;
use sea_orm::ActiveValue;
use time::OffsetDateTime;
use toolkit_odata::ast::{CompareOperator, Expr, Value as OdataValue};
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, SortDir};
use toolkit_security::access_scope::ScopeValue;
use toolkit_security::{
    AccessScope, InTenantSubtreeScopeFilter, ScopeConstraint, ScopeFilter, pep_properties,
};
use uuid::Uuid;

use account_management::infra::storage::entity::tenants;
use common::*;

// ---- helpers ---------------------------------------------------------

/// `InTenantSubtree(root)` with a `descendant_status IN (active)` list,
/// respecting (`respect = true`) or relaxing barriers.
fn status_scope(root: Uuid, respect: bool) -> AccessScope {
    AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::InTenantSubtree(
        InTenantSubtreeScopeFilter::with_descendant_status(
            pep_properties::RESOURCE_ID,
            root,
            respect,
            vec![ScopeValue::Int(i64::from(ACTIVE))],
        ),
    )]))
}

fn ts_at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000 + secs).expect("epoch + offset")
}

/// `$filter=startswith(name,'<prefix>')`.
fn startswith_name(prefix: &str) -> ODataQuery {
    ODataQuery::default().with_filter(Expr::Function(
        "startswith".to_owned(),
        vec![
            Expr::Identifier("name".to_owned()),
            Expr::Value(OdataValue::String(prefix.to_owned())),
        ],
    ))
}

/// `$filter=name eq '<name>'`.
fn name_eq(name: &str) -> ODataQuery {
    ODataQuery::default().with_filter(Expr::Compare(
        Box::new(Expr::Identifier("name".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(OdataValue::String(name.to_owned()))),
    ))
}

/// `$filter=status eq '<label>'`.
fn status_eq(label: &str) -> ODataQuery {
    ODataQuery::default().with_filter(Expr::Compare(
        Box::new(Expr::Identifier("status".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(OdataValue::String(label.to_owned()))),
    ))
}

fn ids_of(items: &[account_management::domain::tenant::model::TenantModel]) -> Vec<Uuid> {
    items.iter().map(|m| m.id).collect()
}

/// Seed an active managed tenant with an explicit `created_at`, its
/// self-row and one `barrier = 0` closure row per strict ancestor in
/// `ancestors` (root first, direct parent last) — the full closure
/// chain the integrity invariants require. `ancestors.last()` is the
/// parent.
async fn seed_visible_at(
    h: &Harness,
    ancestors: &[Uuid],
    id: Uuid,
    name: &str,
    depth: i32,
    created_at: OffsetDateTime,
) {
    use toolkit_db::secure::secure_insert;
    let parent_id = *ancestors.last().expect("at least the root");
    let conn = h.provider.conn().expect("conn");
    let am = tenants::ActiveModel {
        id: ActiveValue::Set(id),
        parent_id: ActiveValue::Set(Some(parent_id)),
        name: ActiveValue::Set(name.to_owned()),
        status: ActiveValue::Set(ACTIVE),
        self_managed: ActiveValue::Set(false),
        tenant_type_uuid: ActiveValue::Set(Uuid::nil()),
        depth: ActiveValue::Set(depth),
        created_at: ActiveValue::Set(created_at),
        updated_at: ActiveValue::Set(created_at),
        deleted_at: ActiveValue::Set(None),
        retention_window_secs: ActiveValue::Set(None),
        claimed_by: ActiveValue::Set(None),
        claimed_at: ActiveValue::Set(None),
        terminal_failure_at: ActiveValue::Set(None),
    };
    secure_insert::<tenants::Entity>(am, &allow_all(), &conn)
        .await
        .expect("seed tenant");
    insert_closure(&h.provider, id, id, 0, ACTIVE)
        .await
        .expect("self-row");
    for a in ancestors {
        insert_closure(&h.provider, *a, id, 0, ACTIVE)
            .await
            .expect("(ancestor, id)");
    }
}

// ---- repo: visible set ----------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_from_root_returns_children_of_respect_visible_nodes() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");

    let page = h
        .repo
        .list_descendants(
            &respect_scope(t.root),
            &relaxed_scope(t.root),
            t.root,
            &ODataQuery::default(),
        )
        .await
        .expect("list");

    assert_eq!(
        sorted(ids_of(&page.items)),
        sorted(vec![t.x, t.xc, t.s, t.y]),
        "rows whose parent is Respect-visible from root: x, xc (via x), s and y as \
         self-managed identities; never yc or sc (their parents sit past a barrier)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_follows_a_barrier_ignoring_scope() {
    // The parent set is gated by the caller's PDP scope, not by a
    // hard-coded `barrier = 0`: under a barrier-ignoring scope rooted at
    // `root`, iterating `/children` reaches `sc` (via `s`) and `yc` (via
    // `y`), so the recursive listing must too.
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");

    let page = h
        .repo
        .list_descendants(
            &relaxed_scope(t.root),
            &relaxed_scope(t.root),
            t.root,
            &ODataQuery::default(),
        )
        .await
        .expect("list");

    assert_eq!(
        sorted(ids_of(&page.items)),
        sorted(vec![t.x, t.xc, t.s, t.sc, t.y, t.yc])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_status_constrained_scope_hides_children_of_hidden_parent() {
    // Codex review counterexample: root(active) → p(suspended) → hit(active).
    // Under a PDP scope that admits only `active` descendants, `p` is not
    // visible, so iterating `/children` can never reach `hit`; the
    // recursive listing must agree and return neither.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::from_u128(0x7300_0001);
    let p = Uuid::from_u128(0x7300_0002);
    let hit = Uuid::from_u128(0x7300_0003);
    insert_tenant(&h.provider, root, None, "root", ACTIVE, false, 0)
        .await
        .expect("root");
    insert_closure(&h.provider, root, root, 0, ACTIVE)
        .await
        .expect("(root, root)");
    insert_tenant(&h.provider, p, Some(root), "p", SUSPENDED, false, 1)
        .await
        .expect("p");
    insert_closure(&h.provider, p, p, 0, SUSPENDED)
        .await
        .expect("(p, p)");
    insert_closure(&h.provider, root, p, 0, SUSPENDED)
        .await
        .expect("(root, p)");
    insert_tenant(&h.provider, hit, Some(p), "hit", ACTIVE, false, 2)
        .await
        .expect("hit");
    insert_closure(&h.provider, hit, hit, 0, ACTIVE)
        .await
        .expect("(hit, hit)");
    insert_closure(&h.provider, p, hit, 0, ACTIVE)
        .await
        .expect("(p, hit)");
    insert_closure(&h.provider, root, hit, 0, ACTIVE)
        .await
        .expect("(root, hit)");

    let constrained = h
        .repo
        .list_descendants(
            &status_scope(root, true),
            &status_scope(root, false),
            root,
            &ODataQuery::default(),
        )
        .await
        .expect("constrained");
    assert!(
        constrained.items.is_empty(),
        "p is status-hidden, so neither p nor its child may be listed; got {} rows",
        constrained.items.len()
    );

    // Without the status list both rows are visible (suspended is not
    // hidden by the hidden-status default).
    let plain = h
        .repo
        .list_descendants(
            &respect_scope(root),
            &relaxed_scope(root),
            root,
            &ODataQuery::default(),
        )
        .await
        .expect("plain");
    assert_eq!(sorted(ids_of(&plain.items)), sorted(vec![p, hit]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_from_intermediate_is_bounded_to_that_subtree() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");

    let page = h
        .repo
        .list_descendants(
            &respect_scope(t.root),
            &relaxed_scope(t.root),
            t.x,
            &ODataQuery::default(),
        )
        .await
        .expect("list");

    assert_eq!(sorted(ids_of(&page.items)), sorted(vec![t.xc, t.y]));
}

// ---- repo: OData surface --------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_contains_name_matches_across_levels() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");

    // Names containing "c": xc, sc, yc. Only xc's parent is visible.
    let page = h
        .repo
        .list_descendants(
            &respect_scope(t.root),
            &relaxed_scope(t.root),
            t.root,
            &contains_name("c"),
        )
        .await
        .expect("list");

    assert_eq!(ids_of(&page.items), vec![t.xc]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_contains_is_ascii_case_insensitive_on_sqlite() {
    // The contract states it per backend: SQLite's `LIKE` ignores ASCII
    // case, Postgres does not (pinned in `list_descendants_integration_pg`).
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");

    let upper = h
        .repo
        .list_descendants(
            &respect_scope(t.root),
            &relaxed_scope(t.root),
            t.root,
            &contains_name("X"),
        )
        .await
        .expect("upper");
    assert_eq!(sorted(ids_of(&upper.items)), sorted(vec![t.x, t.xc]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_startswith_and_eq_on_name() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");

    // startswith 'x': x and xc (yc, sc start with other letters).
    let prefixed = h
        .repo
        .list_descendants(
            &respect_scope(t.root),
            &relaxed_scope(t.root),
            t.root,
            &startswith_name("x"),
        )
        .await
        .expect("startswith");
    assert_eq!(sorted(ids_of(&prefixed.items)), sorted(vec![t.x, t.xc]));

    // eq 'y': the self-managed identity y, two levels down.
    let exact = h
        .repo
        .list_descendants(
            &respect_scope(t.root),
            &relaxed_scope(t.root),
            t.root,
            &name_eq("y"),
        )
        .await
        .expect("eq");
    assert_eq!(ids_of(&exact.items), vec![t.y]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_hides_deleted_by_default_and_shows_on_explicit_filter() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");
    // A soft-deleted grandchild under x, closure rows carry status 3.
    let xd = Uuid::from_u128(0x7000_0010);
    insert_tenant(&h.provider, xd, Some(t.x), "xd", DELETED, false, 2)
        .await
        .expect("seed xd");
    insert_closure(&h.provider, xd, xd, 0, DELETED)
        .await
        .expect("(xd, xd)");
    insert_closure(&h.provider, t.x, xd, 0, DELETED)
        .await
        .expect("(x, xd)");
    insert_closure(&h.provider, t.root, xd, 0, DELETED)
        .await
        .expect("(root, xd)");

    let default_page = h
        .repo
        .list_descendants(
            &respect_scope(t.root),
            &relaxed_scope(t.root),
            t.root,
            &ODataQuery::default(),
        )
        .await
        .expect("default");
    assert!(
        !ids_of(&default_page.items).contains(&xd),
        "hidden-status default must exclude the soft-deleted row"
    );

    let deleted_page = h
        .repo
        .list_descendants(
            &respect_scope(t.root),
            &relaxed_scope(t.root),
            t.root,
            &status_eq("deleted"),
        )
        .await
        .expect("deleted");
    assert_eq!(ids_of(&deleted_page.items), vec![xd]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_orderby_name_desc_spans_levels() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::from_u128(0x7100_0001);
    insert_tenant(&h.provider, root, None, "root", ACTIVE, false, 0)
        .await
        .expect("root");
    insert_closure(&h.provider, root, root, 0, ACTIVE)
        .await
        .expect("(root, root)");
    let a = Uuid::from_u128(0x7100_0002);
    let b = Uuid::from_u128(0x7100_0003);
    let c = Uuid::from_u128(0x7100_0004);
    // Insertion order is NOT name order, and `charlie` sits one level deeper.
    seed_visible_at(&h, &[root], b, "bravo", 1, ts_at(1)).await;
    seed_visible_at(&h, &[root], a, "alpha", 1, ts_at(2)).await;
    seed_visible_at(&h, &[root, a], c, "charlie", 2, ts_at(3)).await;

    let query = ODataQuery::default().with_order(ODataOrderBy(vec![OrderKey {
        field: "name".to_owned(),
        dir: SortDir::Desc,
    }]));
    let page = h
        .repo
        .list_descendants(&respect_scope(root), &relaxed_scope(root), root, &query)
        .await
        .expect("list");

    assert_eq!(ids_of(&page.items), vec![c, b, a]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_cursor_walk_covers_whole_subtree_once() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::from_u128(0x7200_0001);
    insert_tenant(&h.provider, root, None, "root", ACTIVE, false, 0)
        .await
        .expect("root");
    insert_closure(&h.provider, root, root, 0, ACTIVE)
        .await
        .expect("(root, root)");
    // Five visible rows over three levels, strictly increasing created_at.
    let ids: Vec<Uuid> = (0..5u128)
        .map(|i| Uuid::from_u128(0x7200_0010 + i))
        .collect();
    seed_visible_at(&h, &[root], ids[0], "n0", 1, ts_at(1)).await;
    seed_visible_at(&h, &[root], ids[1], "n1", 1, ts_at(2)).await;
    seed_visible_at(&h, &[root, ids[0]], ids[2], "n2", 2, ts_at(3)).await;
    seed_visible_at(&h, &[root, ids[0]], ids[3], "n3", 2, ts_at(4)).await;
    seed_visible_at(&h, &[root, ids[0], ids[2]], ids[4], "n4", 3, ts_at(5)).await;

    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut q = ODataQuery::default().with_limit(2);
        if let Some(c) = cursor.as_deref() {
            q = q.with_cursor(CursorV1::decode(c).expect("decode cursor"));
        }
        let page = h
            .repo
            .list_descendants(&respect_scope(root), &relaxed_scope(root), root, &q)
            .await
            .expect("page");
        seen.extend(ids_of(&page.items));
        pages += 1;
        assert!(pages <= 4, "walk must terminate");
        match page.page_info.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3, "5 rows at limit=2 -> 3 pages");
    assert_eq!(
        seen, ids,
        "default (created_at ASC, id ASC) order, no loss, no duplicate"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_list_descendants_cursor_survives_created_at_collision() {
    // Three rows sharing one `created_at`; only the `id ASC` tiebreaker
    // orders them. `limit = 1` forces a cursor boundary between every
    // pair of colliding rows.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::from_u128(0x7400_0001);
    insert_tenant(&h.provider, root, None, "root", ACTIVE, false, 0)
        .await
        .expect("root");
    insert_closure(&h.provider, root, root, 0, ACTIVE)
        .await
        .expect("(root, root)");
    let ids: Vec<Uuid> = (0..3u128)
        .map(|i| Uuid::from_u128(0x7400_0010 + i))
        .collect();
    seed_visible_at(&h, &[root], ids[0], "c0", 1, ts_at(7)).await;
    seed_visible_at(&h, &[root], ids[1], "c1", 1, ts_at(7)).await;
    seed_visible_at(&h, &[root, ids[0]], ids[2], "c2", 2, ts_at(7)).await;

    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut q = ODataQuery::default().with_limit(1);
        if let Some(c) = cursor.as_deref() {
            q = q.with_cursor(CursorV1::decode(c).expect("decode cursor"));
        }
        let page = h
            .repo
            .list_descendants(&respect_scope(root), &relaxed_scope(root), root, &q)
            .await
            .expect("page");
        seen.extend(ids_of(&page.items));
        pages += 1;
        assert!(pages <= 4, "walk must terminate");
        match page.page_info.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(
        seen, ids,
        "equal timestamps fall back to id ASC; nothing lost or repeated"
    );
}

// ---- repo: ancestor chains ------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_ancestor_chains_returns_intermediates_ordered_top_down() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");
    // One more level: xcc under xc, so a chain has two intermediates.
    let xcc = Uuid::from_u128(0x7000_0011);
    seed_visible_at(&h, &[t.root, t.x, t.xc], xcc, "xcc", 3, ts_at(9)).await;

    let chains = h
        .repo
        .ancestor_chains(&allow_all(), 0, &[t.x, t.xc, t.y, xcc])
        .await
        .expect("chains");

    assert!(
        !chains.contains_key(&t.x),
        "a direct child has no intermediates"
    );
    let xc_chain: Vec<Uuid> = chains[&t.xc].iter().map(|a| a.id).collect();
    assert_eq!(xc_chain, vec![t.x]);
    let y_chain: Vec<Uuid> = chains[&t.y].iter().map(|a| a.id).collect();
    assert_eq!(y_chain, vec![t.x]);
    let deep_chain: Vec<(Uuid, u32)> = chains[&xcc].iter().map(|a| (a.id, a.depth)).collect();
    assert_eq!(deep_chain, vec![(t.x, 1), (t.xc, 2)], "top-down, by depth");
    assert_eq!(chains[&xcc][1].name, "xc");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_ancestor_chains_stops_at_the_listing_root_depth() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");

    // Listing rooted at `x` (depth 1): xc's chain relative to x is empty.
    let chains = h
        .repo
        .ancestor_chains(&allow_all(), 1, &[t.xc])
        .await
        .expect("chains");
    assert!(chains.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_ancestor_chains_empty_input_is_empty() {
    let h = setup_sqlite().await.expect("sqlite");
    let chains = h
        .repo
        .ancestor_chains(&allow_all(), 0, &[])
        .await
        .expect("chains");
    assert!(chains.is_empty());
}

// ---- service: end to end over the barrier topology -------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_list_descendants_from_root_returns_visible_subtree_with_chains() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");
    let services = build_services(&h);

    let page = services
        .tenant_service
        .list_descendants(&ctx_for(t.root), t.root, &ODataQuery::default())
        .await
        .expect("list");

    let by_id: HashMap<Uuid, &account_management_sdk::TenantNode> =
        page.items.iter().map(|n| (n.tenant.id.0, n)).collect();
    assert_eq!(
        sorted(by_id.keys().copied().collect()),
        sorted(vec![t.x, t.xc, t.s, t.y])
    );
    let chain = |id: Uuid| -> Vec<Uuid> { by_id[&id].ancestors.iter().map(|a| a.id.0).collect() };
    assert_eq!(chain(t.x), Vec::<Uuid>::new());
    assert_eq!(chain(t.s), Vec::<Uuid>::new());
    assert_eq!(chain(t.xc), vec![t.x]);
    assert_eq!(chain(t.y), vec![t.x]);
    assert_eq!(by_id[&t.xc].ancestors[0].name, "x");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_list_descendants_child_count_is_barrier_gated() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");
    let services = build_services(&h);

    let page = services
        .tenant_service
        .list_descendants(&ctx_for(t.root), t.root, &ODataQuery::default())
        .await
        .expect("list");
    let counts: HashMap<Uuid, u32> = page
        .items
        .iter()
        .map(|n| (n.tenant.id.0, n.tenant.child_count))
        .collect();

    assert_eq!(counts[&t.x], 2, "x: xc and its self-managed direct child y");
    assert_eq!(counts[&t.xc], 0);
    assert_eq!(
        counts[&t.s], 0,
        "s is an identity past its own barrier: nothing below leaks"
    );
    assert_eq!(counts[&t.y], 0, "same for y");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_list_descendants_from_intermediate_lists_its_subtree_only() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");
    let services = build_services(&h);

    let page = services
        .tenant_service
        .list_descendants(&ctx_for(t.root), t.x, &ODataQuery::default())
        .await
        .expect("list");

    let ids: Vec<Uuid> = page.items.iter().map(|n| n.tenant.id.0).collect();
    assert_eq!(sorted(ids), sorted(vec![t.xc, t.y]));
    assert!(
        page.items.iter().all(|n| n.ancestors.is_empty()),
        "both are direct children of x"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_list_descendants_past_barrier_root_collapses_to_not_found() {
    let h = setup_sqlite().await.expect("sqlite");
    let t = BarrierTopology::new();
    seed_barrier_topology(&h.provider, &t).await.expect("seed");
    let services = build_services(&h);

    let err = services
        .tenant_service
        .list_descendants(&ctx_for(t.root), t.s, &ODataQuery::default())
        .await
        .expect_err("s is past root's barrier");
    assert!(
        matches!(err, DomainError::NotFound { .. }),
        "list_descendants(s) expected NotFound; got {err:?}"
    );
}
