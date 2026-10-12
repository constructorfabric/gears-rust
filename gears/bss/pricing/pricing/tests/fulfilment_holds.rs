//! Frozen acceptance F07 and fresh fulfilment through the production SDK providers.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod acceptance_support;
mod plan_support;
mod seam_support;
use acceptance_support::AcceptanceFixture;
use bss_pricing::domain::commercial_terms::{validate_activation_window, validate_hold_time};
use bss_pricing_sdk::{
    acceptance::{
        AcceptanceReceipt, CommandMeta, HeldBindings, PricingAcceptanceV1, SellabilityV1,
    },
    read::{CatalogRef, PriceModel, PricingReadV1, ResolveQuery},
};
use std::sync::atomic::Ordering;
use time::OffsetDateTime;
use uuid::Uuid;

#[test]
fn a_backdated_activation_cannot_extend_acceptance() {
    let deadline = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    assert!(validate_hold_time(deadline - time::Duration::nanoseconds(1), deadline).is_ok());
    assert_eq!(
        validate_hold_time(deadline, deadline).unwrap_err().code,
        "HOLD_EXPIRED"
    );
}

#[test]
fn workflow_delay_within_the_accepted_window_is_valid() {
    let start = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    let deadline = start + time::Duration::hours(24);
    assert!(validate_activation_window(start, start, deadline).is_ok());
    assert!(
        validate_activation_window(start, start + time::Duration::minutes(3), deadline).is_ok()
    );
    assert!(
        validate_activation_window(start, start - time::Duration::nanoseconds(1), deadline)
            .is_err()
    );
    assert!(validate_activation_window(start, deadline, deadline).is_err());
}
fn meta(key: &str) -> CommandMeta {
    CommandMeta {
        idempotency_key: key.into(),
    }
}
async fn accept(f: &AcceptanceFixture) -> AcceptanceReceipt {
    f.sellability
        .check(&f.ctx, f.query.clone(), f.meta.clone())
        .await
        .unwrap()
}
async fn hold(f: &AcceptanceFixture, a: &AcceptanceReceipt, key: &str) -> HeldBindings {
    f.acceptance
        .hold(&f.ctx, AcceptanceFixture::fulfilment_query(a), meta(key))
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
#[tokio::test]
async fn submit_at_ten_first_hold_at_ten_three_and_exact_replay() {
    let f = fixture().await;
    assert_eq!(f.query.start_at.hour(), 10);
    let a = accept(&f).await;
    f.clock.advance(time::Duration::minutes(3));
    let mut q = AcceptanceFixture::fulfilment_query(&a);
    q.activation_at += time::Duration::minutes(3);
    let eligibility = f
        .sellability
        .check_fulfilment(&f.ctx, q.clone())
        .await
        .unwrap();
    assert_eq!(eligibility.checked_at, q.activation_at);
    assert_eq!(eligibility.valid_before, a.hold_until);
    let first = f
        .acceptance
        .hold(&f.ctx, q.clone(), meta("hold"))
        .await
        .unwrap();
    assert_eq!(first.activation_at, q.activation_at);
    assert_eq!(first.bindings, a.bindings);
    assert_eq!(
        f.acceptance.hold(&f.ctx, q, meta("hold")).await.unwrap(),
        first
    );
}
#[tokio::test]
async fn f07_successor_and_deprecation_keep_exact_original_bindings() {
    let f = fixture().await;
    let a = accept(&f).await;
    let b = &a.bindings[0];
    assert_eq!(b.sku_version, 3);
    assert_eq!(
        b.price.model,
        PriceModel::PerUnit {
            unit_amount: 10.into()
        }
    );
    let mut content = f.catalog.content(b.sku_id);
    content.name = "SKU v4".into();
    content.gl_code = Some("new-gl".into());
    content.invoice_line_template = Some("v4 {sku}".into());
    content.tax_category = Some("standard".into());
    content.billing_timing = Some(bss_products_sdk::models::BillingTiming::Arrears);
    f.catalog.version(b.sku_id, 4, "2026-10-02", content);
    f.execute("UPDATE pricing_price SET effective_to='2026-10-02',version=version+1")
        .await;
    let successor = seam_support::put(
        &f.fixture,
        b.price_book_entry_id,
        seam_support::Row {
            price: serde_json::json!({"rate":"12"}),
            from: "2026-10-02",
            version_no: 2,
            ..seam_support::Row::default()
        },
    )
    .await;
    f.catalog
        .skus
        .lock()
        .unwrap()
        .get_mut(&b.sku_id)
        .unwrap()
        .lifecycle = bss_products_sdk::models::Lifecycle::Deprecated;
    // The successor boundary is inside the acceptance's original 24-hour window.
    *f.clock.0.lock() = seam_support::date("2026-10-02").midnight().assume_utc();
    assert!(*f.clock.0.lock() < a.hold_until);
    let next = f
        .read
        .resolve(
            &f.ctx,
            ResolveQuery {
                catalog: CatalogRef {
                    tenant_id: f.ctx.subject_tenant_id(),
                },
                revision_id: a.query.plan_revision_id,
                date: seam_support::date("2026-10-02"),
                item_id: None,
                pins: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        next.cells[0].binding.as_ref().unwrap().price.price_id,
        successor
    );
    assert_eq!(
        next.cells[0].binding.as_ref().unwrap().price.model,
        PriceModel::PerUnit {
            unit_amount: 12.into()
        }
    );
    let first = hold(&f, &a, "f07").await;
    assert_eq!(first.bindings, a.bindings);
    assert_eq!(first.bindings[0].price_book_entry_id, b.price_book_entry_id);
    assert_eq!(first.bindings[0].usage_rating_policy, b.usage_rating_policy);
    assert_eq!(first.bindings[0].invoice, b.invoice);
}

async fn fixture() -> AcceptanceFixture {
    let mut f = AcceptanceFixture::new().await;
    f.query.start_at += time::Duration::hours(10);
    f.clock.advance(time::Duration::hours(10));
    f
}

#[tokio::test]
async fn expiry_after_successful_hold_replays_only_exact_commands_and_history() {
    let f = fixture().await;
    let a = accept(&f).await;
    let first = hold(&f, &a, "original").await;
    *f.clock.0.lock() = a.hold_until;
    assert_eq!(hold(&f, &a, "original").await, first);
    let q = AcceptanceFixture::fulfilment_query(&a);
    reason(
        &f.sellability
            .check_fulfilment(&f.ctx, q.clone())
            .await
            .unwrap_err(),
        "HoldExpired",
    );
    reason(
        &f.acceptance
            .hold(&f.ctx, q, meta("new-key"))
            .await
            .unwrap_err(),
        "HoldExpired",
    );
    assert_eq!(
        f.acceptance
            .acceptance(&f.ctx, receipt_query(&a))
            .await
            .unwrap(),
        a
    );
    assert_eq!(accept(&f).await, a);
}
fn receipt_query(a: &AcceptanceReceipt) -> bss_pricing_sdk::acceptance::AcceptanceQuery {
    bss_pricing_sdk::acceptance::AcceptanceQuery {
        catalog: CatalogRef {
            tenant_id: a.query.tenant_axes.seller_tenant_id,
        },
        acceptance_id: a.acceptance_id,
    }
}
#[tokio::test]
async fn new_key_keeps_original_hold_and_deadline_and_activation_is_pinned() {
    let f = fixture().await;
    let a = accept(&f).await;
    let first = hold(&f, &a, "one").await;
    f.clock.advance(time::Duration::hours(12));
    assert_eq!(hold(&f, &a, "two").await, first);
    assert_eq!(
        f.acceptance
            .acceptance(&f.ctx, receipt_query(&a))
            .await
            .unwrap(),
        a
    );
    *f.clock.0.lock() = a.hold_until;
    reason(
        &f.sellability
            .check_fulfilment(&f.ctx, AcceptanceFixture::fulfilment_query(&a))
            .await
            .unwrap_err(),
        "HoldExpired",
    );
    *f.clock.0.lock() = a.accepted_at;
    let mut changed = AcceptanceFixture::fulfilment_query(&a);
    changed.activation_at += time::Duration::minutes(1);
    reason(
        &f.acceptance
            .hold(&f.ctx, changed.clone(), meta("one"))
            .await
            .unwrap_err(),
        "IdempotencyConflict",
    );
    reason(
        &f.acceptance
            .hold(&f.ctx, changed.clone(), meta("three"))
            .await
            .unwrap_err(),
        "AcceptanceMismatch",
    );
    reason(
        &f.sellability
            .check_fulfilment(&f.ctx, changed)
            .await
            .unwrap_err(),
        "AcceptanceMismatch",
    );
}
#[tokio::test]
async fn backdated_and_deadline_activation_and_expired_first_hold_are_refused() {
    let f = fixture().await;
    let a = accept(&f).await;
    for at in [
        a.query.start_at - time::Duration::nanoseconds(1),
        a.hold_until,
    ] {
        let mut q = AcceptanceFixture::fulfilment_query(&a);
        q.activation_at = at;
        reason(
            &f.sellability
                .check_fulfilment(&f.ctx, q.clone())
                .await
                .unwrap_err(),
            "ActivationOutsideAcceptedWindow",
        );
        reason(
            &f.acceptance
                .hold(&f.ctx, q, meta("invalid"))
                .await
                .unwrap_err(),
            "ActivationOutsideAcceptedWindow",
        );
    }
    *f.clock.0.lock() = a.hold_until;
    reason(
        &f.acceptance
            .hold(
                &f.ctx,
                AcceptanceFixture::fulfilment_query(&a),
                meta("expired"),
            )
            .await
            .unwrap_err(),
        "HoldExpired",
    );
    assert_eq!(hold_counts(&f).await, (0, 0));
}
#[tokio::test]
async fn exact_market_axes_digest_and_authorization_are_required_even_for_replay() {
    let f = fixture().await;
    let a = accept(&f).await;
    let first = hold(&f, &a, "one").await;
    for field in 0..6 {
        let mut q = AcceptanceFixture::fulfilment_query(&a);
        let expected = match field {
            0 => {
                q.current_market.currency = "USD".into();
                "MarketChanged"
            }
            1 => {
                q.current_market.region = Some("elsewhere".into());
                "MarketChanged"
            }
            2 => {
                q.tenant_axes.payer_tenant_id = uuid::Uuid::new_v4();
                "AcceptanceMismatch"
            }
            3 => {
                q.tenant_axes.resource_tenant_id = uuid::Uuid::new_v4();
                "AcceptanceMismatch"
            }
            4 => {
                q.acceptance.terms_digest[0] ^= 1;
                "AcceptanceMismatch"
            }
            _ => {
                q.acceptance.acceptance_id = uuid::Uuid::new_v4();
                "ReceiptNotFound"
            }
        };
        reason(
            &f.sellability
                .check_fulfilment(&f.ctx, q.clone())
                .await
                .unwrap_err(),
            expected,
        );
        reason(
            &f.acceptance
                .hold(&f.ctx, q.clone(), meta("other"))
                .await
                .unwrap_err(),
            expected,
        );
        reason(
            &f.acceptance.hold(&f.ctx, q, meta("one")).await.unwrap_err(),
            if field == 5 {
                "ReceiptNotFound"
            } else {
                "IdempotencyConflict"
            },
        );
    }
    let mut q = AcceptanceFixture::fulfilment_query(&a);
    q.tenant_axes.seller_tenant_id = uuid::Uuid::new_v4();
    assert_eq!(
        f.sellability
            .check_fulfilment(&f.ctx, q)
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        f.acceptance
            .hold(
                &f.denied_ctx,
                AcceptanceFixture::fulfilment_query(&a),
                meta("one")
            )
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(hold(&f, &a, "one").await, first);
}
#[tokio::test]
async fn retired_sku_refuses_fresh_checks_without_rewriting_history() {
    let f = fixture().await;
    let a = accept(&f).await;
    let historical = f.resolved().await;
    f.catalog
        .skus
        .lock()
        .unwrap()
        .get_mut(&a.bindings[0].sku_id)
        .unwrap()
        .lifecycle = bss_products_sdk::models::Lifecycle::Retired;
    let q = AcceptanceFixture::fulfilment_query(&a);
    reason(
        &f.sellability
            .check_fulfilment(&f.ctx, q.clone())
            .await
            .unwrap_err(),
        "SkuRetired",
    );
    reason(
        &f.acceptance
            .hold(&f.ctx, q, meta("retired"))
            .await
            .unwrap_err(),
        "SkuRetired",
    );
    assert_eq!(f.resolved().await, historical);
    assert_eq!(
        f.acceptance
            .acceptance(&f.ctx, receipt_query(&a))
            .await
            .unwrap(),
        a
    );
}
#[tokio::test]
async fn explicit_close_and_temporary_end_bound_server_and_activation_at_midnight() {
    for temporary in [false, true] {
        let f = fixture().await;
        let a = accept(&f).await;
        let price_id = a.bindings[0].price.price_id;
        let before = f
            .read
            .price(
                &f.ctx,
                bss_pricing_sdk::read::PriceQuery {
                    catalog: receipt_query(&a).catalog.clone(),
                    price_id,
                },
            )
            .await
            .unwrap();
        // New sales reject promotions; this injects authoritative current closing metadata
        // after a real acceptance to exercise defensive fulfilment of an original price.
        let change = if temporary {
            "temporary_until='2026-10-02'"
        } else {
            "closed_explicitly=TRUE"
        };
        f.execute(&format!(
            "UPDATE pricing_price SET {change},effective_to='2026-10-02',version=version+1"
        ))
        .await;
        let end = seam_support::date("2026-10-02").midnight().assume_utc();
        let mut q = AcceptanceFixture::fulfilment_query(&a);
        let eligible = f
            .sellability
            .check_fulfilment(&f.ctx, q.clone())
            .await
            .unwrap();
        assert_eq!(eligible.valid_before, end);
        *f.clock.0.lock() = end - time::Duration::nanoseconds(1);
        assert!(
            f.sellability
                .check_fulfilment(&f.ctx, q.clone())
                .await
                .is_ok()
        );
        q.activation_at = end;
        reason(
            &f.acceptance
                .hold(&f.ctx, q.clone(), meta("end"))
                .await
                .unwrap_err(),
            "PriceClosed",
        );
        q.activation_at = a.query.start_at;
        *f.clock.0.lock() = end;
        reason(
            &f.sellability
                .check_fulfilment(&f.ctx, q.clone())
                .await
                .unwrap_err(),
            "PriceClosed",
        );
        reason(
            &f.acceptance
                .hold(&f.ctx, q, meta("backdated"))
                .await
                .unwrap_err(),
            "PriceClosed",
        );
        let after = f
            .read
            .price(
                &f.ctx,
                bss_pricing_sdk::read::PriceQuery {
                    catalog: receipt_query(&a).catalog,
                    price_id,
                },
            )
            .await
            .unwrap();
        assert_eq!(before.money_digest, after.money_digest);
        assert_eq!(before.model, after.model);
        assert_eq!(
            f.acceptance
                .acceptance(&f.ctx, receipt_query(&a))
                .await
                .unwrap(),
            a
        );
    }
}
#[tokio::test]
async fn provider_outage_and_denial_fail_fresh_checks_but_exact_hold_replays() {
    let f = fixture().await;
    let a = accept(&f).await;
    let first = hold(&f, &a, "one").await;
    f.catalog.down.store(true, Ordering::SeqCst);
    let q = AcceptanceFixture::fulfilment_query(&a);
    assert_eq!(
        f.sellability
            .check_fulfilment(&f.ctx, q.clone())
            .await
            .unwrap_err()
            .status_code(),
        503
    );
    assert_eq!(
        f.acceptance
            .hold(&f.ctx, q.clone(), meta("two"))
            .await
            .unwrap_err()
            .status_code(),
        503
    );
    assert_eq!(hold(&f, &a, "one").await, first);
    f.catalog.down.store(false, Ordering::SeqCst);
    f.catalog.readers([]);
    assert_eq!(
        f.sellability
            .check_fulfilment(&f.ctx, q.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        f.acceptance
            .hold(&f.ctx, q, meta("two"))
            .await
            .unwrap_err()
            .status_code(),
        403
    );
}
async fn hold_counts(f: &AcceptanceFixture) -> (i64, i64) {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let db = Database::connect(&*f.fixture.dsn).await.unwrap();
    let row = db.query_one_raw(Statement::from_string(DbBackend::Sqlite, "SELECT (SELECT count(*) FROM pricing_hold) AS h,(SELECT count(*) FROM pricing_commercial_command WHERE operation='hold') AS c")).await.unwrap().unwrap();
    (row.try_get("", "h").unwrap(), row.try_get("", "c").unwrap())
}
#[tokio::test]
async fn concurrent_holds_have_one_durable_winner_and_one_activation() {
    let f = fixture().await;
    let a = accept(&f).await;
    let q = AcceptanceFixture::fulfilment_query(&a);
    let (one, two) = tokio::join!(
        f.acceptance.hold(&f.ctx, q.clone(), meta("one")),
        f.acceptance.hold(&f.ctx, q.clone(), meta("one"))
    );
    assert_eq!(one.unwrap(), two.unwrap());
    assert_eq!(hold_counts(&f).await, (1, 1));
    let (one, two) = tokio::join!(
        f.acceptance.hold(&f.ctx, q.clone(), meta("two")),
        f.acceptance.hold(&f.ctx, q, meta("three"))
    );
    assert_eq!(one.unwrap(), two.unwrap());
    assert_eq!(hold_counts(&f).await, (1, 3));
    let mut another = f.query.clone();
    another.order_version = 2;
    let a = f
        .sellability
        .check(&f.ctx, another, meta("accept-2"))
        .await
        .unwrap();
    let q = AcceptanceFixture::fulfilment_query(&a);
    let mut other = q.clone();
    other.activation_at += time::Duration::minutes(1);
    let (one, two) = tokio::join!(
        f.acceptance.hold(&f.ctx, q, meta("four")),
        f.acceptance.hold(&f.ctx, other, meta("five"))
    );
    assert_ne!(one.is_ok(), two.is_ok());
    reason(&one.err().or(two.err()).unwrap(), "AcceptanceMismatch");
    assert_eq!(hold_counts(&f).await, (2, 4));
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}
#[test]
fn crash_rolls_back_hold_and_command_and_restart_replays_committed_receipt() {
    let (first, a, dsn, ctx, catalog, clock) = runtime().block_on(async {
        let f = fixture().await;
        let a = accept(&f).await;
        f.execute("CREATE TRIGGER crash_hold BEFORE INSERT ON pricing_commercial_command WHEN NEW.operation='hold' BEGIN SELECT RAISE(ABORT,'response crash'); END").await;
        assert!(f.acceptance.hold(&f.ctx, AcceptanceFixture::fulfilment_query(&a), meta("hold")).await.is_err());
        assert_eq!(hold_counts(&f).await, (0, 0));
        f.execute("DROP TRIGGER crash_hold").await;
        let first = hold(&f, &a, "hold").await;
        (first, a, f.fixture.dsn.clone(), f.ctx, f.catalog, f.clock)
    });
    runtime().block_on(async {
        let db = toolkit_db::DBProvider::new(
            toolkit_db::connect_db(&dsn, toolkit_db::ConnectOpts::default())
                .await
                .unwrap(),
        );
        let state = plan_support::entry_support::state_on(db, catalog.clone()).await;
        catalog.down.store(true, Ordering::SeqCst);
        clock.advance(time::Duration::hours(25));
        let service = std::sync::Arc::new(
            bss_pricing::infra::commercial_terms::CommercialTermsService::new(
                state,
                std::sync::Arc::new(plan_support::entry_support::enforcer_for(
                    ctx.subject_tenant_id(),
                )),
                clock,
                bss_pricing::config::SellerHoldPolicy::default(),
            ),
        );
        let provider =
            bss_pricing::api::pricing_acceptance::PricingAcceptanceProvider::new(service.clone());
        assert_eq!(
            provider
                .hold(&ctx, AcceptanceFixture::fulfilment_query(&a), meta("hold"))
                .await
                .unwrap(),
            first
        );
        let sell = bss_pricing::api::sellability::SellabilityProvider::new(service);
        reason(
            &sell
                .check_fulfilment(&ctx, AcceptanceFixture::fulfilment_query(&a))
                .await
                .unwrap_err(),
            "HoldExpired",
        );
    });
}

async fn publish_replacement(f: &AcceptanceFixture) -> (Uuid, Uuid) {
    use bss_pricing::infra::storage::{
        entity::plan_revision,
        repo::{plan_revision_repo, price_book_entry_repo},
    };
    let tenant = f.ctx.subject_tenant_id();
    let scope = crate::plan_support::scope(&f.fixture);
    let conn = f.fixture.db.conn().unwrap();
    let old = plan_revision_repo::find(&conn, &scope, tenant, f.query.plan_revision_id)
        .await
        .unwrap()
        .unwrap();
    let binding = f.resolved().await.cells[0].binding.clone().unwrap();
    let entry = price_book_entry_repo::find(&conn, &scope, tenant, binding.price_book_entry_id)
        .await
        .unwrap()
        .unwrap();
    let policy = crate::plan_support::entry_support::policy_support::input();
    let mut content: bss_pricing::infra::usage_policy_wire::UsageRatingPolicyInput =
        serde_json::from_value(policy).unwrap();
    content.rating_window = bss_pricing::infra::usage_policy_wire::RatingWindow::CalendarHour {
        timezone: bss_pricing::infra::usage_policy_wire::Timezone::Utc,
    };
    let policy = bss_pricing::infra::storage::repo::usage_policy_repo::intern(
        &conn,
        &scope,
        tenant,
        f.ctx.subject_id(),
        &content,
        f.query.start_at,
    )
    .await
    .unwrap();
    let new_entry = crate::plan_support::entry_with_policy(
        &f.fixture,
        entry.book_id,
        entry.sku_id,
        "usage",
        None,
        "per_unit",
        Some(policy),
    )
    .await;
    crate::seam_support::put(
        &f.fixture,
        new_entry,
        crate::seam_support::Row {
            price: serde_json::json!({"rate":"12"}),
            ..Default::default()
        },
    )
    .await;
    let id = Uuid::new_v4();
    plan_revision_repo::insert(
        &conn,
        &scope,
        plan_revision::Model {
            id,
            rev_no: 2,
            state: "draft".into(),
            available_from: Some(f.query.start_at.date()),
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            ..old
        },
    )
    .await
    .unwrap();
    crate::plan_support::item(&f.fixture, id, entry.sku_id, Some(new_entry), "paid").await;
    crate::plan_support::publish(&f.fixture, f.query.plan_id, id).await;
    (id, new_entry)
}

#[tokio::test]
async fn successor_revision_with_different_window_cannot_replace_accepted_entry() {
    let f = fixture().await;
    let a = accept(&f).await;
    let (revision, entry) = publish_replacement(&f).await;
    let resolved = f
        .read
        .resolve(
            &f.ctx,
            ResolveQuery {
                catalog: receipt_query(&a).catalog,
                revision_id: revision,
                date: a.query.start_at.date(),
                item_id: None,
                pins: vec![],
            },
        )
        .await
        .unwrap();
    let next = resolved.cells[0].binding.as_ref().unwrap();
    assert_eq!(next.price_book_entry_id, entry);
    assert_ne!(
        next.usage_rating_policy.as_ref().unwrap().digest,
        a.bindings[0].usage_rating_policy.as_ref().unwrap().digest
    );
    assert_ne!(next.price.price_id, a.bindings[0].price.price_id);
    assert_eq!(hold(&f, &a, "old-revision").await.bindings, a.bindings);
}

mod fulfilment_support;
#[tokio::test]
async fn off_sale_allows_old_descriptors_and_no_meter_or_dated_sku_refresh() {
    let f = fixture().await;
    let a = accept(&f).await;
    let dated = f.catalog.version_readers.lock().unwrap().len();
    let meter =
        std::sync::Arc::new(plan_support::entry_support::policy_support::MeterProvider::default());
    meter.failure.store(1, Ordering::SeqCst);
    f.fixture
        .state
        .hub
        .register::<dyn bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1>(meter);
    let probe = fulfilment_support::Probe::install(&f, None, None, false);
    assert_eq!(hold(&f, &a, "off-sale").await.bindings, a.bindings);
    assert!(probe.calls.load(Ordering::SeqCst) > 0);
    let readers = f.catalog.version_readers.lock().unwrap().len();
    assert_eq!(readers, dated);
}
#[tokio::test]
async fn hold_recaptures_local_drift_and_samples_commit_clock_with_bounded_retry() {
    for repeat in [false, true] {
        let f = fixture().await;
        let a = accept(&f).await;
        let probe = fulfilment_support::Probe::install(
            &f,
            Some("UPDATE pricing_price SET version=version+1"),
            None,
            repeat,
        );
        let result = f
            .acceptance
            .hold(
                &f.ctx,
                AcceptanceFixture::fulfilment_query(&a),
                meta("drift"),
            )
            .await;
        if repeat {
            reason(&result.unwrap_err(), "ResolutionChanged");
            assert_eq!(
                probe.calls.load(Ordering::SeqCst),
                toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS as usize
            );
            assert_eq!(hold_counts(&f).await, (0, 0));
        } else {
            assert_eq!(result.unwrap().bindings, a.bindings);
            assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
        }
    }
    let f = fixture().await;
    let a = accept(&f).await;
    fulfilment_support::Probe::install(&f, None, Some(time::Duration::hours(24)), false);
    reason(
        &f.acceptance
            .hold(
                &f.ctx,
                AcceptanceFixture::fulfilment_query(&a),
                meta("clock"),
            )
            .await
            .unwrap_err(),
        "HoldExpired",
    );
    assert_eq!(hold_counts(&f).await, (0, 0));
}
#[tokio::test]
async fn explicit_close_during_detached_observation_is_not_admitted() {
    let f = fixture().await;
    let a = accept(&f).await;
    fulfilment_support::Probe::install(
        &f,
        Some(
            "UPDATE pricing_price SET closed_explicitly=TRUE,effective_to='2026-10-01',version=version+1",
        ),
        None,
        false,
    );
    reason(
        &f.acceptance
            .hold(
                &f.ctx,
                AcceptanceFixture::fulfilment_query(&a),
                meta("close-race"),
            )
            .await
            .unwrap_err(),
        "PriceClosed",
    );
    assert_eq!(hold_counts(&f).await, (0, 0));
}

#[tokio::test]
async fn receipt_id_scoped_grant_applies_to_parent_before_child_rows_or_replay() {
    use std::sync::Arc;
    let f = fixture().await;
    let a = accept(&f).await;
    let grant = Arc::new(fulfilment_support::ReceiptGrant {
        acceptance_id: parking_lot::Mutex::new(a.acceptance_id),
        tenant: f.ctx.subject_tenant_id(),
    });
    let service = Arc::new(
        bss_pricing::infra::commercial_terms::CommercialTermsService::new(
            f.fixture.state.clone(),
            Arc::new(authz_resolver_sdk::PolicyEnforcer::new(grant.clone())),
            f.clock.clone(),
            bss_pricing::config::SellerHoldPolicy::default(),
        ),
    );
    let provider =
        bss_pricing::api::pricing_acceptance::PricingAcceptanceProvider::new(service.clone());
    let sell = bss_pricing::api::sellability::SellabilityProvider::new(service);
    let q = AcceptanceFixture::fulfilment_query(&a);
    let first = provider
        .hold(&f.ctx, q.clone(), meta("scoped"))
        .await
        .unwrap();
    assert_eq!(
        provider
            .hold(&f.ctx, q.clone(), meta("another"))
            .await
            .unwrap(),
        first
    );
    assert!(sell.check_fulfilment(&f.ctx, q.clone()).await.is_ok());
    *f.clock.0.lock() = a.hold_until;
    assert_eq!(
        provider
            .hold(&f.ctx, q.clone(), meta("scoped"))
            .await
            .unwrap(),
        first
    );
    *grant.acceptance_id.lock() = Uuid::new_v4();
    assert_eq!(
        provider
            .hold(&f.ctx, q.clone(), meta("scoped"))
            .await
            .unwrap_err()
            .status_code(),
        404
    );
    assert_eq!(
        sell.check_fulfilment(&f.ctx, q)
            .await
            .unwrap_err()
            .status_code(),
        404
    );
    assert_eq!(hold_counts(&f).await, (1, 2));
}

#[path = "review_fix/mod.rs"]
mod review_fix;
