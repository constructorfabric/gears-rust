// Created: 2026-10-18 by Virtuozzo International GmbH
//! A declaration's administrative domain on a real database: its own
//! `domain_affinity`, or its category's when it has none — in the page query
//! and in the per-request check alike.

use std::collections::BTreeSet;

use serde_json::json;
use toolkit_odata::ODataQuery;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::DeclarationRepo;
use crate::domain::category::visibility::DomainVisibility;
use crate::domain::declaration::repo::DeclarationRepository;
use crate::domain::resolution::scope_class;
use crate::test_support::{BOOL, ResolutionHarness};

/// Three declarations: `inherited` with no domain in a `commercial` category,
/// `own_infra` with its own `infrastructure` domain in that same category, and
/// `open` with no domain in the harness's undomained category.
struct Grid {
    h: ResolutionHarness,
    category: Uuid,
    inherited: Uuid,
    own_infra: Uuid,
    open: Uuid,
}

impl Grid {
    async fn over(h: ResolutionHarness) -> Self {
        let category = h.add_category("billing").await;
        h.bind_category_domain(category, Some("commercial")).await;
        let declare = |name: &'static str| {
            h.declare_in(
                category,
                name,
                scope_class::CASCADING,
                json!(false),
                BOOL,
                "public",
            )
        };
        let inherited = declare("inherited").await;
        let own_infra = declare("own_infra").await;
        h.bind_domain(own_infra, "infrastructure").await;
        let open = h
            .declare("open", scope_class::CASCADING, json!(false))
            .await;
        Self {
            h,
            category,
            inherited,
            own_infra,
            open,
        }
    }

    async fn new() -> Self {
        Self::over(ResolutionHarness::new().await).await
    }

    /// What the page query lets through, by leaf name.
    async fn listed(&self, visibility: &DomainVisibility) -> BTreeSet<String> {
        let conn = self.h.db.conn().expect("connection");
        DeclarationRepo
            .list(
                &conn,
                &AccessScope::allow_all(),
                visibility,
                &[],
                &ODataQuery {
                    limit: Some(50),
                    ..ODataQuery::default()
                },
            )
            .await
            .expect("the list")
            .items
            .into_iter()
            .map(|d| d.leaf_slug)
            .collect()
    }

    /// What the load by id and the per-request check let through, by leaf
    /// name, each asserted to agree with the other.
    async fn admitted(&self, visibility: &DomainVisibility) -> BTreeSet<String> {
        let conn = self.h.db.conn().expect("connection");
        let mut out = BTreeSet::new();
        for id in [self.inherited, self.own_infra, self.open] {
            let all = DeclarationRepo
                .find(
                    &conn,
                    &AccessScope::allow_all(),
                    &DomainVisibility::Unrestricted,
                    id,
                )
                .await
                .expect("a read")
                .expect("the declaration");
            let found = DeclarationRepo
                .find(&conn, &AccessScope::allow_all(), visibility, id)
                .await
                .expect("a read");
            let admits = DeclarationRepo
                .admits(&conn, visibility, id, all.domain_affinity.as_deref())
                .await
                .expect("the check");
            assert_eq!(
                found.is_some(),
                admits,
                "{}: the query and the per-request check must agree under {visibility:?}",
                all.leaf_slug
            );
            if admits {
                out.insert(all.leaf_slug);
            }
        }
        out
    }
}

fn restricted(domains: &[&str]) -> DomainVisibility {
    DomainVisibility::Restricted(domains.iter().map(|d| (*d).to_owned()).collect())
}

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|n| (*n).to_owned()).collect()
}

#[tokio::test]
async fn an_undomained_declaration_takes_its_categorys_domain_and_its_own_overrides_it() {
    let g = Grid::new().await;
    for (visibility, expected) in [
        (
            restricted(&["infrastructure"]),
            names(&["own_infra", "open"]),
        ),
        (restricted(&["commercial"]), names(&["inherited", "open"])),
        // The fail-closed reading of an unusable constraint: what has no
        // domain of its own or by its category, and nothing else.
        (restricted(&[]), names(&["open"])),
        (
            DomainVisibility::Unrestricted,
            names(&["inherited", "own_infra", "open"]),
        ),
    ] {
        assert_eq!(
            g.listed(&visibility).await,
            expected,
            "listed under {visibility:?}"
        );
        assert_eq!(
            g.admitted(&visibility).await,
            expected,
            "loaded under {visibility:?}"
        );
    }
}

#[tokio::test]
async fn a_change_of_the_categorys_domain_applies_at_the_next_request() {
    let g = Grid::new().await;
    let infra = restricted(&["infrastructure"]);
    let commercial = restricted(&["commercial"]);

    g.h.bind_category_domain(g.category, None).await;
    assert_eq!(
        g.listed(&infra).await,
        names(&["inherited", "own_infra", "open"])
    );
    assert_eq!(
        g.admitted(&infra).await,
        names(&["inherited", "own_infra", "open"])
    );

    g.h.bind_category_domain(g.category, Some("infrastructure"))
        .await;
    assert_eq!(
        g.listed(&infra).await,
        names(&["inherited", "own_infra", "open"])
    );
    assert_eq!(g.admitted(&commercial).await, names(&["open"]));
}

#[tokio::test]
async fn the_category_is_read_only_when_the_answer_depends_on_it() {
    let (h, recorder) = ResolutionHarness::recorded().await;
    let g = Grid::over(h).await;
    let conn = g.h.db.conn().expect("connection");
    let infra = restricted(&["infrastructure"]);

    recorder.clear();
    assert!(
        DeclarationRepo
            .admits(&conn, &DomainVisibility::Unrestricted, g.inherited, None)
            .await
            .expect("the check")
    );
    assert!(
        DeclarationRepo
            .admits(&conn, &infra, g.own_infra, Some("infrastructure"))
            .await
            .expect("the check")
    );
    assert_eq!(
        recorder.total(),
        0,
        "an unrestricted caller and a declaration with its own domain need no read"
    );

    assert!(
        !DeclarationRepo
            .admits(&conn, &infra, g.inherited, None)
            .await
            .expect("the check")
    );
    assert_eq!(recorder.total(), 1, "one read of the category's domain");
}

#[tokio::test]
async fn a_declaration_that_is_not_there_is_not_admitted() {
    // The per-request check fails closed where the page query returns no row.
    let g = Grid::new().await;
    let conn = g.h.db.conn().expect("connection");
    let missing = Uuid::new_v4();
    assert_eq!(
        DeclarationRepo
            .category_domain(&conn, missing)
            .await
            .expect("a read"),
        None
    );
    assert!(
        !DeclarationRepo
            .admits(&conn, &restricted(&["infrastructure"]), missing, None)
            .await
            .expect("the check")
    );
    assert_eq!(
        DeclarationRepo
            .category_domain(&conn, g.open)
            .await
            .expect("a read"),
        Some(None),
        "a declaration in an undomained category is there, with no domain to take"
    );
}

#[tokio::test]
async fn a_cached_value_is_admitted_on_its_categorys_domain_as_it_stands_now() {
    // The settings read decides on a value the cache may hold, which carries
    // the declaration's own domain only; the category's is read at the
    // request, so a change of it applies to a value cached before.
    let g = Grid::new().await;
    let conn = g.h.db.conn().expect("connection");
    let commercial = restricted(&["commercial"]);
    let infra = restricted(&["infrastructure"]);
    let root = crate::domain::resolution::ScopeTarget::Platform;
    let cached = g.h.resolve("inherited", root).await.expect("resolves");
    let again = g.h.resolve("inherited", root).await.expect("resolves");
    assert!(
        std::sync::Arc::ptr_eq(&cached, &again),
        "the second read is served from the cache"
    );
    assert_eq!(
        cached.domain_affinity, None,
        "the declaration has no domain of its own"
    );
    let admits = |visibility: DomainVisibility| {
        let (conn, resolver, cached) = (&conn, &g.h.resolver, &cached);
        async move {
            resolver
                .domain_admits(
                    conn,
                    &visibility,
                    cached.declaration_id,
                    cached.domain_affinity.as_deref(),
                )
                .await
                .expect("the check")
        }
    };
    assert!(admits(commercial.clone()).await);
    assert!(!admits(infra.clone()).await);

    g.h.bind_category_domain(g.category, Some("infrastructure"))
        .await;
    assert!(!admits(commercial).await);
    assert!(admits(infra).await);
}
