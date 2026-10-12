//! Identical SQLite/PostgreSQL provider scenarios; receipts are issued by the real services.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
use crate::{
    acceptance_support::{AcceptanceFixture, schedule_replacement},
    plan_support as p, seam_support as s,
};
use bss_pricing::infra::storage::entity::{price_book_entry, usage_rating_policy};
use bss_pricing::infra::storage::repo::plan_revision_repo;
use bss_pricing_sdk::{
    acceptance::{AcceptanceQuery, CommandMeta, PricingAcceptanceV1, SellabilityV1},
    read::{CatalogRef, PlanQuery, PricingReadV1},
};
use sea_orm::EntityTrait;
use std::sync::Arc;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

pub async fn fixture(db: DBProvider<DbError>, dsn: p::entry_support::TestDsn) -> AcceptanceFixture {
    let catalog = Arc::new(p::Catalog::default());
    let f = p::Fixture::on(db, Uuid::new_v4(), dsn, catalog.clone()).await;
    AcceptanceFixture::on(f, catalog).await
}
pub async fn accept(f: &AcceptanceFixture) -> bss_pricing_sdk::acceptance::AcceptanceReceipt {
    f.sellability
        .check(&f.ctx, f.query.clone(), f.meta.clone())
        .await
        .unwrap()
}
fn reason(e: &toolkit_canonical_errors::CanonicalError, expected: &str) {
    assert_eq!(
        bss_pricing::infra::commercial_terms::errors::commercial_reason(e).as_deref(),
        Some(expected),
        "{e:?}"
    );
}
pub async fn replay_and_conflicts(f: AcceptanceFixture) {
    let first = accept(&f).await;
    f.clock.advance(time::Duration::hours(25));
    assert_eq!(accept(&f).await, first);
    let mut changed = f.query.clone();
    changed.quantity = 2.into();
    reason(
        &f.sellability
            .check(&f.ctx, changed.clone(), f.meta.clone())
            .await
            .unwrap_err(),
        "IdempotencyConflict",
    );
    let meta = CommandMeta {
        idempotency_key: "another-command".into(),
    };
    reason(
        &f.sellability
            .check(&f.ctx, changed, meta.clone())
            .await
            .unwrap_err(),
        "AcceptanceMismatch",
    );
    assert_eq!(
        f.sellability
            .check(&f.ctx, f.query.clone(), meta)
            .await
            .unwrap(),
        first
    );
    assert_eq!(s::commercial_counts(&f.fixture).await, (1, 2));
}
pub async fn authorization_denial(f: AcceptanceFixture) {
    let first = accept(&f).await;
    let fq = AcceptanceFixture::fulfilment_query(&first);
    assert_eq!(
        f.sellability
            .check(&f.denied_ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        f.acceptance
            .acceptance(
                &f.denied_ctx,
                AcceptanceQuery {
                    catalog: CatalogRef {
                        tenant_id: f.ctx.subject_tenant_id()
                    },
                    acceptance_id: first.acceptance_id
                }
            )
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        f.acceptance
            .hold(&f.denied_ctx, fq.clone(), f.meta.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        f.sellability
            .check_fulfilment(&f.denied_ctx, fq)
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(s::commercial_counts(&f.fixture).await, (1, 1));
}
pub async fn explicit_price_close_race(f: AcceptanceFixture) {
    f.hook(
        vec!["UPDATE pricing_price SET closed_explicitly=TRUE,version=version+1".into()],
        None,
        false,
    );
    reason(
        &f.sellability
            .check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap_err(),
        "PriceClosed",
    );
    assert_eq!(s::commercial_counts(&f.fixture).await, (0, 0));
}
pub async fn money_digest_stability(f: AcceptanceFixture) {
    let first = accept(&f).await;
    let price = first.bindings[0].price.clone();
    f.execute("UPDATE pricing_price SET price_json='{\"rate\":\"10.0000\"}',version=version+1")
        .await;
    let resolved = f.resolved().await;
    assert_eq!(
        resolved.cells[0]
            .binding
            .as_ref()
            .unwrap()
            .price
            .money_digest,
        price.money_digest
    );
    assert_eq!(
        bss_pricing_sdk::digest::money_digest(&price),
        price.money_digest
    );
    assert_eq!(accept(&f).await, first);
}
pub async fn scheduled_promotion_and_held_policy(f: AcceptanceFixture) {
    let first = accept(&f).await;
    let fq = AcceptanceFixture::fulfilment_query(&first);
    let held = f
        .acceptance
        .hold(
            &f.ctx,
            fq.clone(),
            CommandMeta {
                idempotency_key: "hold".into(),
            },
        )
        .await
        .unwrap();
    let (revision, entry) = schedule_replacement(&f).await;
    f.execute(&format!(
        "UPDATE pricing_plan_revision SET available_from='{}' WHERE state='scheduled'",
        time::OffsetDateTime::now_utc().date()
    ))
    .await;
    let q = PlanQuery {
        catalog: CatalogRef {
            tenant_id: f.ctx.subject_tenant_id(),
        },
        plan_id: f.query.plan_id,
    };
    let (a, b) = tokio::join!(
        f.read.current_revision(&f.ctx, q.clone()),
        f.read.current_revision(&f.ctx, q)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_promoted(&f, revision).await;
    let tenant = f.ctx.subject_tenant_id();
    assert_ne!(entry, first.bindings[0].price_book_entry_id);
    let successor = f
        .read
        .resolve(
            &f.ctx,
            bss_pricing_sdk::read::ResolveQuery {
                catalog: CatalogRef { tenant_id: tenant },
                revision_id: revision,
                date: f.query.start_at.date(),
                item_id: None,
                pins: vec![],
            },
        )
        .await
        .unwrap();
    let successor = successor.cells[0].binding.as_ref().unwrap();
    assert_ne!(
        successor.usage_rating_policy,
        first.bindings[0].usage_rating_policy
    );
    assert_ne!(
        successor.price.money_digest,
        first.bindings[0].price.money_digest
    );
    let reread = f
        .acceptance
        .hold(
            &f.ctx,
            fq,
            CommandMeta {
                idempotency_key: "hold".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(held, reread);
    let fresh = f
        .acceptance
        .hold(
            &f.ctx,
            AcceptanceFixture::fulfilment_query(&first),
            CommandMeta {
                idempotency_key: "fresh-hold-after-promotion".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(held, fresh);
    assert_eq!(held.bindings, first.bindings);
    assert_eq!(accept(&f).await, first);
}

async fn assert_promoted(f: &AcceptanceFixture, revision: Uuid) {
    let conn = f.fixture.db.conn().unwrap();
    let scope = p::scope(&f.fixture);
    let tenant = f.ctx.subject_tenant_id();
    assert_eq!(
        plan_revision_repo::find(&conn, &scope, tenant, revision)
            .await
            .unwrap()
            .unwrap()
            .state,
        "published"
    );
    assert_eq!(
        plan_revision_repo::find(&conn, &scope, tenant, f.query.plan_revision_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "superseded"
    );
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
/// Fail after receipt/command writes but before commit, then lose a successful response.
/// Each phase destroys its runtime (including outbox tasks), and the next phase opens a new pool.
pub fn restart(dsn: &p::entry_support::TestDsn) {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let (ctx, query, meta, catalog, clock) = runtime().block_on(async {
        let f=fixture(open(dsn).await,dsn.clone()).await;
        let raw=Database::connect(dsn).await.unwrap(); let backend=raw.get_database_backend();
        if backend==DbBackend::Postgres {
            raw.execute_raw(Statement::from_string(backend,"CREATE FUNCTION crash_acceptance() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.subject_kind = 'acceptance' THEN RAISE EXCEPTION 'simulated crash'; END IF; RETURN NEW; END $$")).await.unwrap();
            f.execute("CREATE TRIGGER crash_acceptance BEFORE INSERT ON pricing_audit FOR EACH ROW EXECUTE FUNCTION crash_acceptance()").await;
        } else {
            f.execute("CREATE TRIGGER crash_acceptance BEFORE INSERT ON pricing_audit WHEN NEW.subject_kind='acceptance' BEGIN SELECT RAISE(ABORT,'simulated crash'); END").await;
        }
        assert!(f.sellability.check(&f.ctx,f.query.clone(),f.meta.clone()).await.is_err());
        assert_eq!(s::commercial_counts(&f.fixture).await,(0,0));
        (f.ctx,f.query,f.meta,f.catalog,f.clock)
    });
    let first = runtime().block_on(async {
        let raw = Database::connect(dsn).await.unwrap();
        let backend = raw.get_database_backend();
        let sql = if backend == DbBackend::Postgres {
            "DROP TRIGGER crash_acceptance ON pricing_audit"
        } else {
            "DROP TRIGGER crash_acceptance"
        };
        raw.execute_raw(Statement::from_string(backend, sql))
            .await
            .unwrap();
        let state = p::entry_support::state_on(open(dsn).await, catalog.clone()).await;
        let service = Arc::new(
            bss_pricing::infra::commercial_terms::CommercialTermsService::new(
                state,
                Arc::new(p::entry_support::enforcer_for(ctx.subject_tenant_id())),
                clock.clone(),
                bss_pricing::config::SellerHoldPolicy::default(),
            ),
        );
        bss_pricing::api::sellability::SellabilityProvider::new(service)
            .check(&ctx, query.clone(), meta.clone())
            .await
            .unwrap()
    });
    runtime().block_on(async {
        catalog
            .down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        clock.advance(time::Duration::hours(25));
        let state = p::entry_support::state_on(open(dsn).await, catalog).await;
        let service = Arc::new(
            bss_pricing::infra::commercial_terms::CommercialTermsService::new(
                state,
                Arc::new(p::entry_support::enforcer_for(ctx.subject_tenant_id())),
                clock,
                bss_pricing::config::SellerHoldPolicy::default(),
            ),
        );
        let replay = bss_pricing::api::sellability::SellabilityProvider::new(service.clone())
            .check(&ctx, query, meta)
            .await
            .unwrap();
        assert_eq!(first, replay);
        let read = bss_pricing::api::pricing_acceptance::PricingAcceptanceProvider::new(service)
            .acceptance(
                &ctx,
                AcceptanceQuery {
                    catalog: CatalogRef {
                        tenant_id: ctx.subject_tenant_id(),
                    },
                    acceptance_id: first.acceptance_id,
                },
            )
            .await
            .unwrap();
        assert_eq!(first, read);
    });
}
pub async fn open(dsn: &str) -> DBProvider<DbError> {
    DBProvider::new(
        toolkit_db::connect_db(
            dsn,
            toolkit_db::ConnectOpts {
                max_conns: Some(2),
                min_conns: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    )
}

pub mod migration;
/// Competing creates use real doors, reservations, policy intern and entry uniqueness.
pub async fn concurrent_entry_policies(db: DBProvider<DbError>, dsn: p::entry_support::TestDsn) {
    let catalog = Arc::new(p::Catalog::default());
    let sku = catalog.sku(bss_products_sdk::models::SkuType::Usage);
    let f = p::Fixture::on(db, Uuid::new_v4(), dsn, catalog.clone()).await;
    let book = p::book(&f, "race").await;
    let path = format!("/price-books/{book}/entries");
    let policy = p::entry_support::policy_support::input();
    let body = serde_json::json!({"sku_id":sku,"model":"per_unit","usage_rating_policy":policy});
    let barrier = tokio::sync::Barrier::new(2);
    let create = |body, key| {
        let barrier = &barrier;
        let f = &f;
        let path = &path;
        async move {
            barrier.wait().await;
            f.call("POST", path, body, None, Some(key)).await
        }
    };
    let (a, b) = tokio::join!(
        create(body.clone(), "same-a"),
        create(body.clone(), "same-b")
    );
    assert!(
        (a.0 == 201 && b.0 == 409) || (a.0 == 409 && b.0 == 201),
        "{a:?} {b:?}"
    );
    let loser = if a.0 == 409 { &a.1 } else { &b.1 };
    assert!(loser.to_string().contains("ENTRY_KEY_TAKEN"), "{a:?} {b:?}");
    let original = if a.0 == 201 { a.1 } else { b.1 };
    let another = catalog.sku(bss_products_sdk::models::SkuType::Usage);
    let mut a = body.clone();
    a["sku_id"] = serde_json::json!(another);
    let mut b = a.clone();
    b["usage_rating_policy"]["rating_window"] =
        serde_json::json!({"kind":"calendar_hour","timezone":"UTC"});
    let (a, b) = tokio::join!(create(a, "different-a"), create(b, "different-b"));
    assert_eq!(a.0, 201, "{a:?}");
    assert_eq!(b.0, 201, "{b:?}");
    assert_ne!(a.1["id"], b.1["id"]);
    assert_eq!(a.1["usage_rating_policy"], original["usage_rating_policy"]);
    assert_ne!(
        a.1["usage_rating_policy"]["digest"],
        b.1["usage_rating_policy"]["digest"]
    );
    let conn = f.db.conn().unwrap();
    let scope = p::scope(&f);
    assert_eq!(
        usage_rating_policy::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&conn)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        price_book_entry::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&conn)
            .await
            .unwrap()
            .len(),
        3
    );
}
fn usage_create_body(catalog: &p::Catalog) -> serde_json::Value {
    let sku = catalog.sku(bss_products_sdk::models::SkuType::Usage);
    let policy = p::entry_support::policy_support::input();
    serde_json::json!({"sku_id": sku, "model": "per_unit", "usage_rating_policy": policy})
}
/// Tx B fails one transaction budget of retryable contention, then the insert
/// is allowed. The door must still answer 201.
pub async fn entry_write_contention_still_confirms(
    db: DBProvider<DbError>,
    dsn: p::entry_support::TestDsn,
) {
    let catalog = Arc::new(p::Catalog::default());
    let f = p::Fixture::on(db, Uuid::new_v4(), dsn, catalog.clone()).await;
    let book = p::book(&f, "write-fault").await;
    let body = usage_create_body(&catalog);
    let path = format!("/price-books/{book}/entries");
    bss_pricing::infra::reference_work::with_create_faults(3, 0, async {
        let (status, body, _) = f.call("POST", &path, body, None, Some("write-fault")).await;
        assert_eq!(status, 201, "{body}");
        assert_eq!(body["reference_state"], "confirmed");
    })
    .await;
    let conn = f.db.conn().unwrap();
    let scope = p::scope(&f);
    assert_eq!(
        price_book_entry::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&conn)
            .await
            .unwrap()
            .len(),
        1
    );
}
/// The confirm transaction fails one budget after the entry is written. The
/// door must finish the confirm and answer 201, not 409 `CONTENDED`.
pub async fn confirm_contention_still_confirms(
    db: DBProvider<DbError>,
    dsn: p::entry_support::TestDsn,
) {
    let catalog = Arc::new(p::Catalog::default());
    let f = p::Fixture::on(db, Uuid::new_v4(), dsn, catalog.clone()).await;
    let book = p::book(&f, "confirm-fault").await;
    let body = usage_create_body(&catalog);
    let path = format!("/price-books/{book}/entries");
    bss_pricing::infra::reference_work::with_create_faults(0, 3, async {
        let (status, body, _) = f
            .call("POST", &path, body, None, Some("confirm-fault"))
            .await;
        assert_eq!(status, 201, "{body}");
        assert_eq!(body["reference_state"], "confirmed");
    })
    .await;
}
