#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::{change::SkuChange, publish::SkuPublish, retire::SkuRetire};
use crate::{
    infra::{
        broker::{EventSink, SkuChanged, SkuPublished, SkuRetired},
        events,
        storage::repo,
    },
    test_support::*,
};
use bss_approval::{ApprovalError, ApprovalSubject, Engine, Policy, Store, SubmitRequest};
use bss_products_sdk::models::{Lifecycle, SkuContent, SkuType};
use event_broker_sdk::TypedEvent;
use std::sync::Arc;
use toolkit_db::{Db, DbTx};
use uuid::Uuid;

async fn in_tx<T: Send + 'static>(
    db: &Db,
    mut f: impl for<'a> FnMut(
        &'a DbTx<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, ApprovalError>> + Send + 'a>,
    > + Send,
) -> Result<T, crate::api::rest::TxError> {
    db.transaction_with_retry(
        toolkit_db::secure::TxConfig::default(),
        crate::api::rest::contention_db_err,
        move |tx| {
            let future = f(tx);
            Box::pin(async move { future.await.map_err(Into::into) })
        },
    )
    .await
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One committed publish/change/retire lifecycle exercises the actual subjects and outbox"
)]
async fn subjects_publish_change_refuse_corrupt_reference_and_withdraw() {
    let (db, scope, tenant, dsn) = test_db().await;
    let handle = toolkit_db::outbox::Outbox::builder(db.db())
        .table_prefix(events::OUTBOX_TABLE_PREFIX)
        .unwrap()
        .queue(
            events::QUEUE_NAME,
            toolkit_db::outbox::Partitions::of(events::PARTITIONS),
        )
        .leased(events::PendingBrokerProducer)
        .start()
        .await
        .unwrap();
    let conn = db.conn().unwrap();
    let category = repo::insert_category(
        &conn,
        &scope,
        tenant,
        crate::domain::category::NewCategory {
            code: "c".into(),
            name: "C".into(),
            is_default: false,
            sort_order: 0,
        },
        at(9),
    )
    .await
    .unwrap();
    let sku = seed_rest_sku(&conn, &scope, tenant, category.id, "SKU").await;
    let mut content = SkuContent::from(&sku);
    content.r#type = SkuType::Recurring;
    repo::update_sku_draft(&conn, &scope, tenant, sku.id, sku.revision, &content, at(9))
        .await
        .unwrap();
    let base = SkuPublish {
        scope: scope.clone(),
        tenant_id: tenant,
        outbox: events::TxOutbox::new(EventSink::Interim(Arc::clone(handle.outbox()))),
        actor: sku.created_by,
        now: at(9),
        usage_type: None,
    };
    let id = sku.id;
    let submit_base = base.clone();
    let published = in_tx(&db.db(), move |tx| {
        let b = submit_base.clone();
        Box::pin(async move {
            Engine::submit(
                &repo::ProductsApprovalStore {
                    scope: b.scope.clone(),
                    tenant_id: tenant,
                },
                &b,
                tx,
                SubmitRequest {
                    tenant_id: tenant,
                    ref_id: id,
                    item_ids: &[id],
                    actor: b.actor,
                    policy: &Policy {
                        default_quorum: 0,
                        overrides: std::collections::BTreeMap::default(),
                    },
                    common_effective_date: None,
                    note: None,
                    now: b.now,
                },
            )
            .await
        })
    })
    .await
    .ok()
    .unwrap();
    assert!(published.applied);
    let got = repo::find_sku(&conn, &scope, tenant, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.lifecycle, Lifecycle::Published);
    // A delayed non-fenced terminal callback cannot clear a later unit's lock.
    let other_unit = Uuid::new_v4();
    assert!(
        repo::try_lock_sku(&conn, &scope, tenant, id, other_unit, got.revision)
            .await
            .unwrap()
    );
    let stale_subject = base.clone();
    let stale_unit = published.unit.clone();
    let failure = in_tx(&db.db(), move |tx| {
        let b = stale_subject.clone();
        let u = stale_unit.clone();
        Box::pin(async move {
            let store = repo::ProductsApprovalStore {
                scope: b.scope.clone(),
                tenant_id: tenant,
            };
            let items = store.items(tx, u.id).await?;
            Ok(matches!(
                b.unlock(tx, &u, &items, false).await,
                Err(ApprovalError::Store(_))
            ))
        })
    })
    .await;
    assert!(failure.ok().unwrap());
    let locked = repo::find_sku(&conn, &scope, tenant, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(locked.pending_unit_id, Some(other_unit));
    assert_eq!(locked.approved_by_unit_id, Some(published.unit.id));
    assert!(matches!(
        repo::unlock_sku(&conn, &scope, tenant, id, other_unit, None)
            .await
            .unwrap(),
        repo::HeadWrite::Written(_)
    ));
    assert_eq!(got.published_version, 1);
    assert_eq!(enqueued_event_count(&dsn, SkuPublished::TYPE_ID).await, 1);
    assert_eq!(
        repo::versions(&conn, &scope, tenant, id)
            .await
            .unwrap()
            .len(),
        1
    );
    let change = SkuChange {
        base: base.clone(),
        patch: crate::domain::sku::SkuPatch {
            gl_code: Some(Some("4012".into())),
            ..Default::default()
        },
        effective_from: utc(2026, 10, 1, 0, 0, 0).date(),
        fence_op_id: None,
    };
    in_tx(&db.db(), move |tx| {
        let s = change.clone();
        Box::pin(async move {
            Engine::submit(
                &repo::ProductsApprovalStore {
                    scope: s.base.scope.clone(),
                    tenant_id: tenant,
                },
                &s,
                tx,
                SubmitRequest {
                    tenant_id: tenant,
                    ref_id: id,
                    item_ids: &[id],
                    actor: s.base.actor,
                    policy: &Policy {
                        default_quorum: 0,
                        overrides: std::collections::BTreeMap::default(),
                    },
                    common_effective_date: Some(s.effective_from),
                    note: None,
                    now: s.base.now,
                },
            )
            .await
        })
    })
    .await
    .ok()
    .unwrap();
    let got = repo::find_sku(&conn, &scope, tenant, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.gl_code.as_deref(), Some("4012"));
    assert_eq!(got.published_version, 2);
    assert_eq!(
        repo::version_as_of(&conn, &scope, tenant, id, utc(2026, 10, 1, 0, 0, 0).date())
            .await
            .unwrap()
            .unwrap()
            .published_version,
        2
    );
    assert_eq!(
        enqueued_event_envelope(&dsn, SkuChanged::TYPE_ID).await["changed"],
        serde_json::json!(["gl_code"])
    );
    // Submit on the requested date, then decide using tomorrow's clock.
    let requested = utc(2026, 10, 1, 9, 0, 0);
    let apply_at = requested + time::Duration::days(1);
    let op = Uuid::new_v4();
    let mut dated_base = base.clone();
    dated_base.now = requested;
    let dated = SkuChange {
        base: dated_base,
        patch: crate::domain::sku::SkuPatch {
            r#type: Some(SkuType::OneTime),
            ..Default::default()
        },
        effective_from: requested.date(),
        fence_op_id: Some(op),
    };
    let submit = dated.clone();
    let pending = in_tx(&db.db(), move |tx| {
        let s = submit.clone();
        Box::pin(async move {
            repo::fence_sku(
                tx,
                &s.base.scope,
                tenant,
                id,
                repo::Fence::TypeChange,
                op,
                requested,
            )
            .await
            .map_err(super::store_err)?;
            Engine::submit(
                &repo::ProductsApprovalStore {
                    scope: s.base.scope.clone(),
                    tenant_id: tenant,
                },
                &s,
                tx,
                SubmitRequest {
                    tenant_id: tenant,
                    ref_id: id,
                    item_ids: &[id],
                    actor: s.base.actor,
                    policy: &Policy {
                        default_quorum: 1,
                        overrides: std::collections::BTreeMap::default(),
                    },
                    common_effective_date: Some(requested.date()),
                    note: None,
                    now: requested,
                },
            )
            .await
        })
    })
    .await
    .ok()
    .unwrap();
    assert_eq!(
        pending.unit.snapshot["effective_from"],
        requested.date().to_string()
    );
    let unit_id = pending.unit.id;
    let mut approved = dated;
    approved.base.now = apply_at;
    in_tx(&db.db(), move |tx| {
        let s = approved.clone();
        Box::pin(async move {
            Engine::approve(
                &repo::ProductsApprovalStore {
                    scope: s.base.scope.clone(),
                    tenant_id: tenant,
                },
                &s,
                tx,
                unit_id,
                Uuid::new_v4(),
                1,
                None,
                apply_at,
            )
            .await
        })
    })
    .await
    .ok()
    .unwrap();
    let versions = repo::versions(&conn, &scope, tenant, id).await.unwrap();
    assert_eq!(versions.last().unwrap().effective_from, apply_at.date());
    assert_eq!(
        repo::version_as_of(&conn, &scope, tenant, id, requested.date())
            .await
            .unwrap()
            .unwrap()
            .published_version,
        2
    );
    assert_eq!(
        enqueued_event_envelope(&dsn, SkuChanged::TYPE_ID).await["effectiveFrom"],
        apply_at.date().to_string()
    );
    assert!(
        !repo::find_sku(&conn, &scope, tenant, id)
            .await
            .unwrap()
            .unwrap()
            .type_change_pending
    );
    assert!(
        repo::find_sku_fence(&conn, &scope, tenant, id)
            .await
            .unwrap()
            .unwrap()
            .fence_op_id
            .is_none()
    );
    let op = Uuid::new_v4();
    let retire = SkuRetire {
        base: base.clone(),
        fence_op_id: op,
    };
    let submitted = in_tx(&db.db(), move |tx| {
        let s = retire.clone();
        Box::pin(async move {
            repo::fence_sku(
                tx,
                &s.base.scope,
                tenant,
                id,
                repo::Fence::Retire,
                op,
                s.base.now,
            )
            .await
            .map_err(super::store_err)?;
            Engine::submit(
                &repo::ProductsApprovalStore {
                    scope: s.base.scope.clone(),
                    tenant_id: tenant,
                },
                &s,
                tx,
                SubmitRequest {
                    tenant_id: tenant,
                    ref_id: id,
                    item_ids: &[id],
                    actor: s.base.actor,
                    policy: &Policy {
                        default_quorum: 1,
                        overrides: std::collections::BTreeMap::default(),
                    },
                    common_effective_date: None,
                    note: None,
                    now: s.base.now,
                },
            )
            .await
        })
    })
    .await
    .ok()
    .unwrap();
    let unit_id = submitted.unit.id;
    repo::reserve_reference(
        &conn,
        &scope,
        tenant,
        id,
        "pricing",
        crate::domain::references::RefKind::PriceBookEntry,
        Uuid::new_v4(),
        base.actor,
        at(10),
    )
    .await
    .unwrap();
    let b = base.clone();
    let failed = in_tx(&db.db(), move |tx| {
        let b = b.clone();
        Box::pin(async move {
            Engine::approve(
                &repo::ProductsApprovalStore {
                    scope: b.scope.clone(),
                    tenant_id: tenant,
                },
                &SkuRetire {
                    base: b,
                    fence_op_id: op,
                },
                tx,
                unit_id,
                Uuid::new_v4(),
                1,
                None,
                at(10),
            )
            .await
        })
    })
    .await;
    assert!(matches!(
        failed,
        Err(crate::api::rest::TxError::Refused(
            crate::domain::error::DomainError::Conflict {
                code: "SKU_REFERENCED",
                ..
            }
        ))
    ));
    assert_eq!(
        repo::find_sku(&conn, &scope, tenant, id)
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        Lifecycle::Published
    );
    assert!(
        repo::find_sku(&conn, &scope, tenant, id)
            .await
            .unwrap()
            .unwrap()
            .retire_pending
    );
    assert_eq!(enqueued_event_count(&dsn, SkuRetired::TYPE_ID).await, 0);
    in_tx(&db.db(), move |tx| {
        let b = base.clone();
        Box::pin(async move {
            let store = repo::ProductsApprovalStore {
                scope: b.scope.clone(),
                tenant_id: tenant,
            };
            let actor = b.actor;
            Engine::withdraw(
                &store,
                &SkuRetire {
                    base: b,
                    fence_op_id: op,
                },
                tx,
                unit_id,
                actor,
                at(11),
            )
            .await?;
            assert_eq!(store.decisions(tx, unit_id).await?.len(), 0);
            Ok(())
        })
    })
    .await
    .ok()
    .unwrap();
    let got = repo::find_sku(&conn, &scope, tenant, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.lifecycle, Lifecycle::Published);
    assert!(got.pending_unit_id.is_none());
    assert!(
        got.approved_by_unit_id.is_some(),
        "withdrawal retains previous approval attribution"
    );
    handle.stop().await;
}

/// P-D-196: a category missing at the store — a race past the subject's own check — is a refusal
/// with its code, 409, never the store's 500; the subject's own refusal names the category id and
/// answers 404, as the draft doors do.
#[test]
fn a_missing_category_is_a_refusal_at_the_store_and_a_404_from_the_subject() {
    use crate::domain::error::DomainError;
    use toolkit::api::canonical_prelude::CanonicalError;
    let raced = super::store_err(crate::infra::storage::RepoError::Refused(
        crate::infra::storage::RepoRefusal::CategoryNotFound,
    ));
    assert!(
        matches!(
            raced,
            ApprovalError::ApplyRefused {
                code: "CATEGORY_NOT_FOUND",
                ..
            }
        ),
        "{raced:?}"
    );
    assert_eq!(
        CanonicalError::from(DomainError::from(raced)).status_code(),
        409
    );
    let id = Uuid::new_v4();
    let named = DomainError::from(ApprovalError::ApplyRefused {
        code: "CATEGORY_NOT_FOUND",
        detail: id.to_string(),
    });
    assert_eq!(
        named,
        DomainError::NotFound {
            what: "category",
            id
        }
    );
    assert_eq!(CanonicalError::from(named).status_code(), 404);
}
