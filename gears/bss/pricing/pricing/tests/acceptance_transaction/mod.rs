//! Real command replay, race, failure and immutable history contracts.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::AcceptanceFixture;
use bss_pricing::{
    api::{pricing_acceptance::PricingAcceptanceProvider, sellability::SellabilityProvider},
    config::SellerHoldPolicy,
};
use bss_pricing_sdk::{
    acceptance::{
        AcceptanceQuery, AcceptanceRef, FulfilmentQuery, PricingAcceptanceV1, SellabilityV1,
    },
    digest::selected_bindings_digest,
    read::{CatalogRef, PricingReadV1, ResolveQuery},
};
use std::sync::{Arc, atomic::Ordering};
use uuid::Uuid;
async fn accept(f: &AcceptanceFixture) -> bss_pricing_sdk::acceptance::AcceptanceReceipt {
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

#[tokio::test]
async fn concurrent_exact_retry_commits_one_receipt_command_and_audit() {
    let f = AcceptanceFixture::new().await;
    let (a, b) = tokio::join!(
        f.sellability.check(&f.ctx, f.query.clone(), f.meta.clone()),
        f.sellability.check(&f.ctx, f.query.clone(), f.meta.clone())
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(f.counts().await, (1, 1, 1));
}
#[tokio::test]
async fn changed_payload_and_new_key_obey_distinct_uniqueness_contracts() {
    let f = AcceptanceFixture::new().await;
    let first = accept(&f).await;
    let mut changed = f.query.clone();
    changed.quantity = 2.into();
    reason(
        &f.sellability
            .check(&f.ctx, changed.clone(), f.meta.clone())
            .await
            .unwrap_err(),
        "IdempotencyConflict",
    );
    let mut meta = f.meta.clone();
    meta.idempotency_key = "new".into();
    reason(
        &f.sellability
            .check(&f.ctx, changed, meta.clone())
            .await
            .unwrap_err(),
        "AcceptanceMismatch",
    );
    f.clock.advance(time::Duration::hours(25));
    assert_eq!(
        f.sellability
            .check(&f.ctx, f.query.clone(), meta)
            .await
            .unwrap(),
        first
    );
    assert_eq!(f.counts().await, (1, 2, 1));
}
#[tokio::test]
async fn caller_identity_scopes_the_same_key() {
    let f = AcceptanceFixture::new().await;
    let first = accept(&f).await;
    let caller = crate::plan_support::entry_support::user_of(f.ctx.subject_tenant_id());
    assert_eq!(
        f.sellability
            .check(&caller, f.query.clone(), f.meta.clone())
            .await
            .unwrap(),
        first
    );
    let mut q = f.query.clone();
    q.line_id = Uuid::new_v4();
    q.quantity = 2.into();
    let third = crate::plan_support::entry_support::user_of(f.ctx.subject_tenant_id());
    assert_ne!(
        f.sellability
            .check(&third, q, f.meta.clone())
            .await
            .unwrap()
            .acceptance_id,
        first.acceptance_id
    );
    assert_eq!(f.counts().await, (2, 3, 2));
}
#[tokio::test]
async fn denied_replay_is_authorized_before_receipt_lookup() {
    let f = AcceptanceFixture::new().await;
    accept(&f).await;
    let denied = toolkit_security::SecurityContext::builder()
        .subject_id(f.ctx.subject_id())
        .subject_tenant_id(f.ctx.subject_tenant_id())
        .subject_type("denied")
        .build()
        .unwrap();
    assert_eq!(
        f.sellability
            .check(&denied, f.query.clone(), f.meta.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(f.counts().await, (1, 1, 1));
}
#[tokio::test]
async fn wrong_tenant_digest_and_incomplete_selection_persist_nothing() {
    let f = AcceptanceFixture::new().await;
    let mut q = f.query.clone();
    q.tenant_axes.seller_tenant_id = Uuid::new_v4();
    assert_eq!(
        f.sellability
            .check(&f.ctx, q, f.meta.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    let mut q = f.query.clone();
    q.resolved_bindings_digest = [0; 32];
    reason(
        &f.sellability
            .check(&f.ctx, q, f.meta.clone())
            .await
            .unwrap_err(),
        "ResolutionChanged",
    );
    let mut q = f.query.clone();
    q.billing_terms.digest = [0; 32];
    reason(
        &f.sellability
            .check(&f.ctx, q, f.meta.clone())
            .await
            .unwrap_err(),
        "BillingTermsDigestMismatch",
    );
    let mut q = f.query.clone();
    q.selections.clear();
    reason(
        &f.sellability
            .check(&f.ctx, q, f.meta.clone())
            .await
            .unwrap_err(),
        "IncompleteSelection",
    );
    assert_eq!(f.counts().await, (0, 0, 0));
    accept(&f).await;
}
#[tokio::test]
async fn a_new_order_version_issues_a_new_receipt() {
    let f = AcceptanceFixture::new().await;
    let first = accept(&f).await;
    let mut q = f.query.clone();
    q.order_version += 1;
    q.quantity = 2.into();
    let mut meta = f.meta.clone();
    meta.idempotency_key = "v2".into();
    let next = f.sellability.check(&f.ctx, q, meta).await.unwrap();
    assert_ne!(next.acceptance_id, first.acceptance_id);
    assert_eq!(f.counts().await, (2, 2, 2));
}
#[tokio::test]
async fn provider_denial_and_outage_do_not_poison_the_command() {
    let f = AcceptanceFixture::new().await;
    let hook = f.hook(vec![], None, false);
    for (failure, status) in [(2, 403), (1, 503)] {
        hook.provider.failure.store(failure, Ordering::SeqCst);
        assert_eq!(
            f.sellability
                .check(&f.ctx, f.query.clone(), f.meta.clone())
                .await
                .unwrap_err()
                .status_code(),
            status
        );
        assert_eq!(f.counts().await, (0, 0, 0));
    }
    hook.provider.failure.store(0, Ordering::SeqCst);
    f.catalog.readers([]);
    assert_eq!(
        f.sellability
            .check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    f.catalog.readers([f.ctx.subject_id()]);
    f.catalog.down.store(true, Ordering::SeqCst);
    assert_eq!(
        f.sellability
            .check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap_err()
            .status_code(),
        503
    );
    f.catalog.down.store(false, Ordering::SeqCst);
    accept(&f).await;
    assert_eq!(f.counts().await, (1, 1, 1));
}
#[tokio::test]
async fn mixed_generation_race_never_freezes_old_money_with_new_invoice_inputs() {
    let f = AcceptanceFixture::new().await;
    f.hook(
        vec![
            "UPDATE pricing_price_book_entry SET invoice_line_override='changed',version=version+1"
                .into(),
            "UPDATE pricing_price SET price_json='{\"rate\":\"12\"}',version=version+1".into(),
        ],
        None,
        false,
    );
    reason(
        &f.sellability
            .check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap_err(),
        "ResolutionChanged",
    );
    assert_eq!(f.counts().await, (0, 0, 0));
    let resolved = f.resolved().await;
    let mut q = f.query.clone();
    q.resolved_bindings_digest = selected_bindings_digest(&resolved, &q.selections).unwrap();
    let r = f
        .sellability
        .check(&f.ctx, q, f.meta.clone())
        .await
        .unwrap();
    assert_eq!(r.bindings[0], resolved.cells[0].binding.clone().unwrap());
}
#[tokio::test]
async fn repeated_generation_drift_is_bounded() {
    let f = AcceptanceFixture::new().await;
    let hook = f.hook(
        vec!["UPDATE pricing_price SET version=version+1".into()],
        None,
        true,
    );
    reason(
        &f.sellability
            .check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap_err(),
        "ResolutionChanged",
    );
    assert_eq!(
        hook.provider.calls.load(Ordering::SeqCst),
        toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS as usize
    );
    assert_eq!(f.counts().await, (0, 0, 0));
}
#[tokio::test]
async fn price_close_race_and_commit_clock_expiry_refuse_acceptance() {
    let f = AcceptanceFixture::new().await;
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
    assert_eq!(f.counts().await, (0, 0, 0));
    let g = AcceptanceFixture::new().await;
    g.execute("UPDATE pricing_price SET effective_to='2026-10-02',version=version+1")
        .await;
    let resolved = g.resolved().await;
    let mut q = g.query.clone();
    q.resolved_bindings_digest = selected_bindings_digest(&resolved, &q.selections).unwrap();
    g.hook(vec![], Some(time::Duration::days(1)), false);
    reason(
        &g.sellability
            .check(&g.ctx, q, g.meta.clone())
            .await
            .unwrap_err(),
        "PriceClosed",
    );
    assert_eq!(g.counts().await, (0, 0, 0));
}
#[tokio::test]
async fn forged_other_entry_price_or_policy_digest_is_rejected() {
    let f = AcceptanceFixture::new().await;
    let resolved = f.resolved().await;
    let (revision, _) = schedule_replacement(&f).await;
    let other = f
        .read
        .resolve(
            &f.ctx,
            ResolveQuery {
                catalog: CatalogRef {
                    tenant_id: f.ctx.subject_tenant_id(),
                },
                revision_id: revision,
                date: f.query.start_at.date() + time::Duration::days(1),
                item_id: None,
                pins: vec![],
            },
        )
        .await
        .unwrap()
        .cells[0]
        .binding
        .clone()
        .unwrap();
    for policy in [false, true] {
        let mut forged = resolved.clone();
        let b = forged.cells[0].binding.as_mut().unwrap();
        if policy {
            b.usage_rating_policy = other.usage_rating_policy.clone();
        } else {
            b.price = other.price.clone();
            b.price_book_entry_id = other.price_book_entry_id;
        }
        let mut q = f.query.clone();
        q.resolved_bindings_digest = selected_bindings_digest(&forged, &q.selections).unwrap();
        reason(
            &f.sellability
                .check(&f.ctx, q, f.meta.clone())
                .await
                .unwrap_err(),
            "ResolutionChanged",
        );
    }
    assert_eq!(f.counts().await, (0, 0, 0));
}

#[tokio::test]
async fn seller_policy_change_preserves_issued_deadline_and_receipt_is_not_fulfilment() {
    let f = AcceptanceFixture::new().await;
    let first = accept(&f).await;
    let service = f.service(SellerHoldPolicy {
        version: std::num::NonZeroU64::new(2).unwrap(),
        duration_seconds: std::num::NonZeroU32::new(3600).unwrap(),
    });
    let sell = SellabilityProvider::new(service.clone());
    assert_eq!(
        sell.check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap(),
        first
    );
    let mut q = f.query.clone();
    q.order_version = 2;
    let mut meta = f.meta.clone();
    meta.idempotency_key = "next".into();
    reason(
        &sell
            .check(&f.ctx, q.clone(), meta.clone())
            .await
            .unwrap_err(),
        "UnsupportedTerms",
    );
    q.hold_policy_version = 2;
    let next = sell.check(&f.ctx, q, meta).await.unwrap();
    assert_eq!(next.hold_until - next.accepted_at, time::Duration::hours(1));
    let fq = FulfilmentQuery {
        tenant_axes: first.query.tenant_axes.clone(),
        acceptance: AcceptanceRef {
            acceptance_id: first.acceptance_id,
            terms_digest: first.terms_digest,
        },
        current_market: first.query.market.clone(),
        activation_at: first.query.start_at,
    };
    assert_eq!(
        sell.check_fulfilment(&f.ctx, fq.clone())
            .await
            .unwrap()
            .valid_before,
        first.hold_until
    );
    // An issued acceptance still requires live retirement validation at fulfilment.
    f.catalog
        .skus
        .lock()
        .unwrap()
        .get_mut(&first.bindings[0].sku_id)
        .unwrap()
        .lifecycle = bss_products_sdk::models::Lifecycle::Retired;
    reason(
        &sell.check_fulfilment(&f.ctx, fq.clone()).await.unwrap_err(),
        "SkuRetired",
    );
    reason(
        &PricingAcceptanceProvider::new(service)
            .hold(&f.ctx, fq, f.meta.clone())
            .await
            .unwrap_err(),
        "SkuRetired",
    );
}
#[test]
fn crash_before_commit_rolls_back_every_row_and_restart_replays_after_commit() {
    let (first, dsn, ctx, query, meta, catalog, clock) = super::restart_runtime().block_on(async {
        let f = AcceptanceFixture::new().await;
        f.execute("CREATE TRIGGER crash_acceptance BEFORE INSERT ON pricing_audit WHEN NEW.subject_kind='acceptance' BEGIN SELECT RAISE(ABORT,'simulated crash'); END").await;
        assert!(f.sellability.check(&f.ctx, f.query.clone(), f.meta.clone()).await.is_err());
        assert_eq!(f.counts().await, (0, 0, 0));
        f.execute("DROP TRIGGER crash_acceptance").await;
        let first = accept(&f).await;
        assert_eq!(f.counts().await, (1, 1, 1));
        (first, f.fixture.dsn.clone(), f.ctx, f.query, f.meta, f.catalog, f.clock)
    });
    // All old providers, pools and outbox tasks have been destroyed with the first runtime.
    super::restart_runtime().block_on(async {
        let state =
            crate::plan_support::entry_support::state_on(super::pool(&dsn).await, catalog.clone())
                .await;
        catalog.down.store(true, Ordering::SeqCst);
        clock.advance(time::Duration::hours(25));
        let service = Arc::new(
            bss_pricing::infra::commercial_terms::CommercialTermsService::new(
                state,
                Arc::new(crate::plan_support::entry_support::enforcer_for(
                    ctx.subject_tenant_id(),
                )),
                clock,
                SellerHoldPolicy::default(),
            ),
        );
        assert_eq!(
            SellabilityProvider::new(service.clone())
                .check(&ctx, query, meta)
                .await
                .unwrap(),
            first
        );
        assert_eq!(
            PricingAcceptanceProvider::new(service)
                .acceptance(
                    &ctx,
                    AcceptanceQuery {
                        catalog: CatalogRef {
                            tenant_id: ctx.subject_tenant_id()
                        },
                        acceptance_id: first.acceptance_id
                    }
                )
                .await
                .unwrap(),
            first
        );
    });
}

use crate::acceptance_support::schedule_replacement;
#[tokio::test]
async fn scheduled_switch_at_commit_recaptures_and_refuses_the_old_revision() {
    let f = AcceptanceFixture::new().await;
    schedule_replacement(&f).await;
    f.hook(vec![], Some(time::Duration::days(1)), false);
    reason(
        &f.sellability
            .check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap_err(),
        "NotSellable",
    );
    assert_eq!(f.counts().await, (0, 0, 0));
}
#[tokio::test]
async fn successor_revision_selecting_another_entry_cannot_rewrite_old_receipt() {
    let f = AcceptanceFixture::new().await;
    let first = accept(&f).await;
    let (revision, entry) = schedule_replacement(&f).await;
    f.clock.advance(time::Duration::days(1));
    let mut q = f.query.clone();
    q.order_version = 2;
    let mut meta = f.meta.clone();
    meta.idempotency_key = "revision-2".into();
    reason(
        &f.sellability
            .check(&f.ctx, q.clone(), meta.clone())
            .await
            .unwrap_err(),
        "NotSellable",
    );
    q.plan_revision_id = revision;
    q.start_at += time::Duration::days(1);
    let resolved = f
        .read
        .resolve(
            &f.ctx,
            ResolveQuery {
                catalog: CatalogRef {
                    tenant_id: f.ctx.subject_tenant_id(),
                },
                revision_id: revision,
                date: q.start_at.date(),
                item_id: None,
                pins: vec![],
            },
        )
        .await
        .unwrap();
    q.selections = resolved.cells.iter().map(|c| c.selection.clone()).collect();
    q.resolved_bindings_digest = selected_bindings_digest(&resolved, &q.selections).unwrap();
    let next = f.sellability.check(&f.ctx, q, meta).await.unwrap();
    assert_eq!(next.bindings[0].price_book_entry_id, entry);
    assert_ne!(
        next.bindings[0].price_book_entry_id,
        first.bindings[0].price_book_entry_id
    );
    assert_eq!(accept(&f).await, first);
    assert_eq!(
        f.acceptance
            .acceptance(
                &f.ctx,
                AcceptanceQuery {
                    catalog: CatalogRef {
                        tenant_id: f.ctx.subject_tenant_id()
                    },
                    acceptance_id: first.acceptance_id
                }
            )
            .await
            .unwrap(),
        first
    );
}

#[tokio::test]
async fn harmless_generation_change_recaptures_fresh_evidence() {
    let f = AcceptanceFixture::new().await;
    let hook = f.hook(
        vec!["UPDATE pricing_price SET version=version+1".into()],
        None,
        false,
    );
    let r = accept(&f).await;
    assert_eq!(
        r.bindings[0],
        f.resolved().await.cells[0].binding.clone().unwrap()
    );
    assert_eq!(hook.provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(f.counts().await, (1, 1, 1));
}
#[tokio::test]
async fn commit_clock_and_utc_equivalent_request_define_the_receipt() {
    let f = AcceptanceFixture::new().await;
    f.hook(vec![], Some(time::Duration::minutes(7)), false);
    let mut q = f.query.clone();
    q.start_at = q
        .start_at
        .to_offset(time::UtcOffset::from_hms(-5, 0, 0).unwrap());
    let r = f
        .sellability
        .check(&f.ctx, q, f.meta.clone())
        .await
        .unwrap();
    assert_eq!(r.accepted_at, f.query.start_at + time::Duration::minutes(7));
    assert_eq!(r.hold_until, r.accepted_at + time::Duration::hours(24));
    assert_eq!(accept(&f).await, r);
}
#[tokio::test]
async fn retired_sku_and_temporary_promotion_cannot_issue_new_acceptance() {
    let f = AcceptanceFixture::new().await;
    let sku = f.resolved().await.cells[0].binding.as_ref().unwrap().sku_id;
    f.catalog
        .age(sku, bss_products_sdk::models::Lifecycle::Retired);
    reason(
        &f.sellability
            .check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap_err(),
        "NotSellable",
    );
    f.catalog
        .age(sku, bss_products_sdk::models::Lifecycle::Published);
    f.execute("UPDATE pricing_price SET temporary_until='2026-10-03',version=version+1")
        .await;
    let resolved = f.resolved().await;
    let mut q = f.query.clone();
    q.resolved_bindings_digest = selected_bindings_digest(&resolved, &q.selections).unwrap();
    reason(
        &f.sellability
            .check(&f.ctx, q, f.meta.clone())
            .await
            .unwrap_err(),
        "UnsupportedTerms",
    );
    assert_eq!(f.counts().await, (0, 0, 0));
}
