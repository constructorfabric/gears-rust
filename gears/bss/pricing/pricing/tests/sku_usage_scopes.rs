//! The SKU usage port's scoped sets (P-D-246, ask 52): `sku_ids_in(Book)` is the SKUs with an
//! entry in the book, in any reference state, under `price_book_entry:read`; `sku_ids_in(Revision)`
//! is the SKUs the revision's items name, under `price_book_entry:read` and `plan:read`. A book or
//! a revision the tenant does not hold is the empty set, a missing grant 403, and each scope one
//! statement whatever its size.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod book_support;
mod entry_paging_support;
mod plan_support;
use book_support::stored_book;
use bss_pricing::api::sku_usage::PricingSkuUsage;
use bss_products_sdk::sku_usage::{SkuUsageV1, UsageScope};
use entry_paging_support::entry_at;
use plan_support::{Catalog, Fixture, entry_support, holding, item, plan, setup};
use std::sync::Arc;
use uuid::Uuid;

fn port(f: &Fixture) -> PricingSkuUsage {
    PricingSkuUsage::new(
        f.state.clone(),
        entry_support::enforcer_for(f.ctx.subject_tenant_id()),
    )
}

fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// A book's SKUs: every SKU with an entry in it, in any reference state, distinct and sorted; a
/// SKU priced only in another book is not one of them.
#[tokio::test]
async fn a_books_skus_are_those_with_an_entry_in_it_in_any_reference_state() {
    let (f, _) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let now = time::OffsetDateTime::now_utc();
    let (eur, usd) = (
        stored_book(&f, "eur", now).await,
        stored_book(&f, "usd", now).await,
    );
    let (a, b, c, elsewhere) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    for (sku, model, state) in [
        (a, "per_unit", "confirmed"),
        (a, "graduated", "lost"),
        (b, "per_unit", "confirmation_pending"),
        (c, "volume", "lost"),
    ] {
        entry_at(&f, eur, Uuid::now_v7(), sku, "usage", None, model, state).await;
    }
    entry_at(
        &f,
        usd,
        Uuid::now_v7(),
        elsewhere,
        "usage",
        None,
        "per_unit",
        "confirmed",
    )
    .await;
    assert_eq!(
        port(&f)
            .sku_ids_in(&f.ctx, tenant, UsageScope::Book(eur))
            .await
            .unwrap(),
        sorted(vec![a, b, c])
    );
    assert_eq!(
        port(&f)
            .sku_ids_in(&f.ctx, tenant, UsageScope::Book(usd))
            .await
            .unwrap(),
        vec![elsewhere]
    );
}

/// A revision's SKUs: every SKU its items name — with an entry in any reference state, or
/// without one — sorted; another revision's items are not its own.
#[tokio::test]
async fn a_revisions_skus_are_those_its_items_name() {
    let (f, _) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let now = time::OffsetDateTime::now_utc();
    let eur = stored_book(&f, "eur", now).await;
    let (a, b, c, other) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let ea = entry_at(
        &f,
        eur,
        Uuid::now_v7(),
        a,
        "usage",
        None,
        "per_unit",
        "confirmed",
    )
    .await;
    let eb = entry_at(&f, eur, Uuid::now_v7(), b, "usage", None, "volume", "lost").await;
    let eo = entry_at(
        &f,
        eur,
        Uuid::now_v7(),
        other,
        "usage",
        None,
        "per_unit",
        "confirmed",
    )
    .await;
    let (_, mine) = plan(&f, "mine", eur).await;
    item(&f, mine, a, Some(ea), "paid").await;
    item(&f, mine, b, Some(eb), "paid").await;
    item(&f, mine, c, None, "included").await;
    let (_, theirs) = plan(&f, "theirs", eur).await;
    item(&f, theirs, other, Some(eo), "paid").await;
    assert_eq!(
        port(&f)
            .sku_ids_in(&f.ctx, tenant, UsageScope::Revision(mine))
            .await
            .unwrap(),
        sorted(vec![a, b, c])
    );
    assert_eq!(
        port(&f)
            .sku_ids_in(&f.ctx, tenant, UsageScope::Revision(theirs))
            .await
            .unwrap(),
        vec![other]
    );
}

/// No existence oracle: a book or a revision the tenant does not hold — unknown, or another
/// tenant's — answers the empty set, as one that names nothing does; naming another tenant does
/// not widen the caller's reach.
#[tokio::test]
async fn a_foreign_or_unknown_book_or_revision_is_the_empty_set() {
    let (f, _) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let eur = stored_book(&f, "eur", time::OffsetDateTime::now_utc()).await;
    let sku = Uuid::new_v4();
    let e = entry_at(
        &f,
        eur,
        Uuid::now_v7(),
        sku,
        "usage",
        None,
        "per_unit",
        "confirmed",
    )
    .await;
    let (_, revision) = plan(&f, "pro", eur).await;
    item(&f, revision, sku, Some(e), "paid").await;
    let (_, empty) = plan(&f, "empty", eur).await;
    let other = Uuid::new_v4();
    let theirs = PricingSkuUsage::new(f.state.clone(), entry_support::enforcer_for(other));
    let stranger = entry_support::user_of(other);
    for scope in [UsageScope::Book(eur), UsageScope::Revision(revision)] {
        assert_eq!(
            port(&f).sku_ids_in(&f.ctx, tenant, scope).await.unwrap(),
            vec![sku],
            "{scope:?}"
        );
        assert!(
            theirs
                .sku_ids_in(&stranger, other, scope)
                .await
                .unwrap()
                .is_empty(),
            "another tenant's reader: {scope:?}"
        );
        assert!(
            port(&f)
                .sku_ids_in(&f.ctx, other, scope)
                .await
                .unwrap()
                .is_empty(),
            "naming another tenant: {scope:?}"
        );
    }
    for scope in [
        UsageScope::Book(Uuid::new_v4()),
        UsageScope::Revision(Uuid::new_v4()),
        UsageScope::Revision(empty),
    ] {
        assert!(
            port(&f)
                .sku_ids_in(&f.ctx, tenant, scope)
                .await
                .unwrap()
                .is_empty(),
            "{scope:?}"
        );
    }
}

/// A revision's SKUs are plan content: the revision scope takes `plan:read` beside
/// `price_book_entry:read`, and the book scope `price_book_entry:read`. Without the grant the port
/// is 403 — never an empty set.
// Probed (PROBE-9-8-2): the revision scope judged under entry read alone.
#[tokio::test]
async fn the_revision_scope_takes_plan_read_beside_entry_read() {
    let (f, _) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let eur = stored_book(&f, "eur", time::OffsetDateTime::now_utc()).await;
    let sku = Uuid::new_v4();
    let e = entry_at(
        &f,
        eur,
        Uuid::now_v7(),
        sku,
        "usage",
        None,
        "per_unit",
        "confirmed",
    )
    .await;
    let (_, revision) = plan(&f, "pro", eur).await;
    item(&f, revision, sku, Some(e), "paid").await;
    let refused = |grant: &str, scope: UsageScope| (grant.to_owned(), scope);
    for (grant, scope) in [
        refused("price_book_entry:read", UsageScope::Revision(revision)),
        refused("plan:read", UsageScope::Revision(revision)),
        refused("plan:read", UsageScope::Book(eur)),
        refused("price_book:read", UsageScope::Book(eur)),
        refused("price_book:read", UsageScope::Revision(revision)),
    ] {
        let error = port(&f)
            .sku_ids_in(&holding(&f, &grant), tenant, scope)
            .await
            .unwrap_err();
        let (status, body, _) = entry_support::answer(Err(error)).await;
        assert_eq!(status, 403, "{grant} {scope:?}: {body}");
    }
    assert_eq!(
        port(&f)
            .sku_ids_in(
                &holding(&f, "price_book_entry:read"),
                tenant,
                UsageScope::Book(eur)
            )
            .await
            .unwrap(),
        vec![sku]
    );
    assert_eq!(
        port(&f)
            .sku_ids_in(&f.ctx, tenant, UsageScope::Revision(revision))
            .await
            .unwrap(),
        vec![sku],
        "a reader with both grants"
    );
}

/// The statements on pricing's tables one call of `sku_ids_in` makes.
async fn scoped_statements(
    f: &Fixture,
    recorder: &toolkit_db::test_support::QueryRecorder,
    scope: UsageScope,
    n: usize,
) -> Vec<(String, usize)> {
    recorder.clear();
    let ids = port(f)
        .sku_ids_in(&f.ctx, f.ctx.subject_tenant_id(), scope)
        .await
        .unwrap();
    assert_eq!(ids.len(), n, "{scope:?}");
    recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("pricing_"))
        })
        .map(|q| (q.sql, q.param_count))
        .collect()
}

/// Each scope is ONE statement, the same with the same binds for 10 and for 100 SKUs.
#[tokio::test]
async fn each_scope_reads_in_one_statement_for_10_and_100_skus() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let f = Fixture::on(db, tenant, dsn, Arc::new(Catalog::default())).await;
    let mut traces = Vec::new();
    for n in [10_usize, 100] {
        let book = stored_book(&f, &format!("b{n}"), time::OffsetDateTime::now_utc()).await;
        let (_, revision) = plan(&f, &format!("p{n}"), book).await;
        for _ in 0..n {
            let sku = Uuid::new_v4();
            let e = entry_at(
                &f,
                book,
                Uuid::now_v7(),
                sku,
                "usage",
                None,
                "per_unit",
                "confirmed",
            )
            .await;
            item(&f, revision, sku, Some(e), "paid").await;
        }
        let by_book = scoped_statements(&f, &recorder, UsageScope::Book(book), n).await;
        let by_revision = scoped_statements(&f, &recorder, UsageScope::Revision(revision), n).await;
        for (i, (sql, binds)) in by_book.iter().chain(&by_revision).enumerate() {
            eprintln!("{n}: statement {i} ({binds} binds): {sql}");
        }
        assert_eq!(by_book.len(), 1, "{by_book:#?}");
        assert_eq!(by_revision.len(), 1, "{by_revision:#?}");
        traces.push((by_book, by_revision));
    }
    assert_eq!(
        traces[0], traces[1],
        "the same statements whatever the size"
    );
}
