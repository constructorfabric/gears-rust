//! The book's entries pager (D-483) and the SKU usage port's scoped sets (P-D-246) on
//! `PostgreSQL`: the pages cross a usage SKU's and a recurring group's boundaries in the order
//! `(sku_id, charge_kind, model, id)` — a `uuid` and two texts ordered and compared by the engine
//! production runs — the filter narrows every page, and each scope reads its SKUs.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod book_support;
mod entry_paging_support;
mod pg_support;
mod plan_support;
use book_support::stored_book;
use bss_pricing::api::sku_usage::PricingSkuUsage;
use bss_products_sdk::sku_usage::{SkuUsageV1, UsageScope};
use entry_paging_support::entry_at;
use plan_support::{Catalog, Fixture, entry_support, holding, item, plan};
use std::sync::Arc;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

async fn fixture(pg: &pg_support::Pg) -> Fixture {
    let db = DBProvider::<DbError>::new(pg.db().await);
    Fixture::on(
        db,
        Uuid::new_v4(),
        plan_support::entry_support::TestDsn::of(pg.url(true)),
        Arc::new(Catalog::default()),
    )
    .await
}

/// D-483 on Postgres: the pages join into the one order at every page size, a cursor keyed on
/// the SKU's `uuid`, the charge kind, the model and the id.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_entries_page_across_usage_and_recurring_boundaries() {
    let pg = pg_support::Pg::applied().await;
    let f = fixture(&pg).await;
    entry_paging_support::the_pages_cross_every_boundary(&f).await;
    // A fixture of its own (a fresh tenant): both scenarios seed the same book code, which one
    // tenant holds once (`BOOK_CODE_TAKEN`), as each runs alone on SQLite.
    let g = fixture(&pg).await;
    entry_paging_support::the_filter_narrows_every_page(&g).await;
}

/// P-D-246 on Postgres: a book's SKUs in any reference state, a revision's SKUs, the empty set
/// for another tenant's, and the revision scope's `plan:read`.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_sku_usage_scopes_read_their_skus() {
    let pg = pg_support::Pg::applied().await;
    let f = fixture(&pg).await;
    let tenant = f.ctx.subject_tenant_id();
    let port = PricingSkuUsage::new(f.state.clone(), entry_support::enforcer_for(tenant));
    let eur = stored_book(&f, "eur", time::OffsetDateTime::now_utc()).await;
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let ea = entry_at(
        &f,
        eur,
        Uuid::now_v7(),
        a,
        "usage",
        None,
        "per_unit",
        "lost",
    )
    .await;
    entry_at(
        &f,
        eur,
        Uuid::now_v7(),
        b,
        "usage",
        None,
        "volume",
        "confirmed",
    )
    .await;
    let (_, revision) = plan(&f, "pro", eur).await;
    item(&f, revision, a, Some(ea), "paid").await;
    let mut both = vec![a, b];
    both.sort_unstable();
    assert_eq!(
        port.sku_ids_in(&f.ctx, tenant, UsageScope::Book(eur))
            .await
            .unwrap(),
        both
    );
    assert_eq!(
        port.sku_ids_in(&f.ctx, tenant, UsageScope::Revision(revision))
            .await
            .unwrap(),
        vec![a]
    );
    let other = Uuid::new_v4();
    for scope in [UsageScope::Book(eur), UsageScope::Revision(revision)] {
        assert!(
            port.sku_ids_in(&f.ctx, other, scope)
                .await
                .unwrap()
                .is_empty(),
            "{scope:?}"
        );
    }
    let refused = port
        .sku_ids_in(
            &holding(&f, "price_book_entry:read"),
            tenant,
            UsageScope::Revision(revision),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.status_code(), 403, "{refused}");
}
