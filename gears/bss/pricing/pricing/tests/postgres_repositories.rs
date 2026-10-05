//! Native Postgres constraints through the same scoped repositories.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod pg_support;
use bss_pricing::infra::storage::{
    RepoError,
    entity::{price, price_book, price_book_entry},
    repo::{book_repo, price_book_entry_repo, price_repo},
};
use toolkit_db::secure::AccessScope;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_unique_codes_and_price_decimal_roundtrip() {
    let pg = pg_support::Pg::applied().await;
    let provider = DBProvider::<DbError>::new(pg.db().await);
    let conn = provider.conn().unwrap();
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let now = time::OffsetDateTime::now_utc();
    let b = price_book::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        code: "standard".into(),
        name: "Standard".into(),
        currency: "EUR".into(),
        valid_from: None,
        valid_until: None,
        description: None,
        version: 1,
        created_at: now,
        updated_at: now,
        archived_at: None,
        archived_by: None,
    };
    book_repo::insert(&conn, &scope, b.clone()).await.unwrap();
    assert!(matches!(
        book_repo::insert(
            &conn,
            &scope,
            price_book::Model {
                id: Uuid::new_v4(),
                ..b.clone()
            }
        )
        .await,
        Err(RepoError::Conflict {
            code: "BOOK_CODE_TAKEN"
        })
    ));
    let p = price_book_entry::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        book_id: b.id,
        sku_id: Uuid::new_v4(),
        charge_kind: "usage".into(),
        period: None,
        model: "per_unit".into(),
        usage_policy_id: None,
        usage_policy_version: None,
        usage_policy_digest: None,
        usage_sku_version: None,
        dimension_key: None,
        invoice_line_override: None,
        reservation_id: Uuid::new_v4(),
        reference_state: "confirmed".into(),
        version: 1,
        created_at: now,
        updated_at: now,
    };
    price_book_entry_repo::insert(&conn, &scope, p.clone())
        .await
        .unwrap();
    assert!(matches!(
        price_book_entry_repo::insert(
            &conn,
            &scope,
            price_book_entry::Model {
                id: Uuid::new_v4(),
                ..p.clone()
            }
        )
        .await,
        Err(RepoError::Conflict {
            code: "ENTRY_KEY_TAKEN"
        })
    ));
    let r = price::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        price_book_entry_id: p.id,
        version_no: 1,
        dim_value: None,
        price_json: serde_json::json!({"rate":"0.123456789"}),
        min_fee: Some("12.34".into()),
        eligibility: "all".into(),
        effective_from: now.date(),
        effective_to: None,
        keep_for_bound: false,
        closed_explicitly: false,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        change_kind: "set".into(),
        target_price_id: None,
        cancelled_by_unit_id: None,
        state: "approved".into(),
        pending_unit_id: None,
        approved_by_unit_id: None,
        note: None,
        created_by: Uuid::new_v4(),
        approved_at: None,
        version: 1,
        created_at: now,
        updated_at: now,
    };
    let got = price_repo::insert(&conn, &scope, r.clone()).await.unwrap();
    assert_eq!(got.price_json, r.price_json);
    assert_eq!(got.min_fee, r.min_fee);
    assert!(matches!(
        price_repo::insert(
            &conn,
            &scope,
            price::Model {
                id: Uuid::new_v4(),
                version_no: 2,
                ..r.clone()
            }
        )
        .await,
        Err(RepoError::Conflict {
            code: "WINDOW_OVERLAP"
        })
    ));
    assert!(matches!(
        price_repo::insert(
            &conn,
            &scope,
            price::Model {
                id: Uuid::new_v4(),
                dim_value: Some("us".into()),
                ..r
            }
        )
        .await,
        Err(RepoError::Conflict {
            code: "PRICE_VERSION_TAKEN"
        })
    ));
}
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_audit_rows_refuse_deletion_and_edits() {
    use bss_pricing::infra::storage::repo::audit_repo::{AuditCommon, write_eventless_act_audit};
    use sea_orm::ConnectionTrait;
    let pg = pg_support::Pg::applied().await;
    let provider = DBProvider::<DbError>::new(pg.db().await);
    let tenant = Uuid::new_v4();
    write_eventless_act_audit(
        &provider.conn().unwrap(),
        &AccessScope::for_tenant(tenant),
        AuditCommon {
            audit_id: Uuid::new_v4(),
            tenant_id: tenant,
            actor_ref: Uuid::new_v4(),
            action: "book.create".into(),
            subject_kind: "price_book".into(),
            reason: None,
            correlation_id: Some("c".into()),
            written_at: time::OffsetDateTime::now_utc(),
        },
        Uuid::new_v4(),
        Some(1),
    )
    .await
    .unwrap();
    let raw = pg.raw().await;
    for statement in [
        "DELETE FROM bss.pricing_audit",
        "UPDATE bss.pricing_audit SET action = 'forged'",
    ] {
        let refused = raw.execute_unprepared(statement).await.unwrap_err();
        assert!(refused.to_string().contains("append-only"), "{refused}");
    }
}
