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
        currency: "EUR".into(),
        currency_scale: 2,
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
/// A book, one of its entries and a draft price of it, as the PostgreSQL column tests write them.
async fn draft_parent(
    conn: &impl toolkit_db::secure::DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
) -> price::Model {
    let now = time::OffsetDateTime::now_utc();
    let b = price_book::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        code: "columns".into(),
        name: "Columns".into(),
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
    book_repo::insert(conn, scope, b.clone()).await.unwrap();
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
    price_book_entry_repo::insert(conn, scope, p.clone())
        .await
        .unwrap();
    price::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        price_book_entry_id: p.id,
        version_no: 1,
        dim_value: None,
        price_json: serde_json::json!({"rate":"0.1"}),
        currency: "EUR".into(),
        currency_scale: 2,
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
        state: "draft".into(),
        pending_unit_id: None,
        approved_by_unit_id: None,
        note: None,
        created_by: Uuid::new_v4(),
        approved_at: None,
        version: 1,
        created_at: now,
        updated_at: now,
    }
}
/// The PostgreSQL twins of the `SQLite` column tests: the `min_fee` CHECK takes canonical
/// unsigned decimal text only, the `currency` CHECK three uppercase letters and the
/// `currency_scale` CHECK 0 to 28. The vectors are the `SQLite` ones; the expressions differ.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_price_columns_refuse_a_non_canonical_fee_and_a_spec_out_of_range() {
    let pg = pg_support::Pg::applied().await;
    let provider = DBProvider::<DbError>::new(pg.db().await);
    let conn = provider.conn().unwrap();
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let draft = draft_parent(&conn, &scope, tenant).await;
    let mut version_no = 0;
    let mut next = || {
        version_no += 1;
        price::Model {
            id: Uuid::new_v4(),
            version_no,
            ..draft.clone()
        }
    };
    for text in [
        "-1", "1.", "1.2.3", "1e5", "", " 3", "30.00", "0.10", "01", "00", "0.0", ".5",
    ] {
        let r = price::Model {
            min_fee: Some(text.into()),
            ..next()
        };
        assert!(
            price_repo::insert(&conn, &scope, r).await.is_err(),
            "the column must refuse a fee that is not canonical unsigned decimal text"
        );
    }
    for text in ["0", "0.5", "30", "12.34", "100.01"] {
        let r = price::Model {
            min_fee: Some(text.into()),
            ..next()
        };
        let stored = price_repo::insert(&conn, &scope, r)
            .await
            .expect("the column takes a canonical fee");
        assert_eq!(
            stored.min_fee.as_deref(),
            Some(text),
            "the fee reads back as written"
        );
    }
    for (code, scale) in [
        ("eur", 2),
        ("EU", 2),
        ("EURO", 2),
        ("E1R", 2),
        ("EUR", -1),
        ("EUR", 29),
    ] {
        let r = price::Model {
            currency: code.into(),
            currency_scale: scale,
            ..next()
        };
        assert!(
            price_repo::insert(&conn, &scope, r).await.is_err(),
            "the columns must refuse a malformed code or a scale outside 0 to 28"
        );
    }
    for scale in [0, 28] {
        let r = price::Model {
            currency_scale: scale,
            ..next()
        };
        let stored = price_repo::insert(&conn, &scope, r).await.unwrap();
        let spec = price_repo::currency_of(&stored).unwrap();
        assert_eq!(
            (spec.code(), i16::from(spec.scale())),
            ("EUR", scale),
            "the stored spec reads back as written"
        );
    }
}
