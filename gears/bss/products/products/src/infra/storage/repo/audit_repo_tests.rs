#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::test_support::{at, test_db};
use sea_orm::{ColumnTrait, Condition};
use toolkit_db::secure::SecureEntityExt;
const TENANT: Uuid = Uuid::from_u128(0x7e_11);
const PRODUCT: Uuid = Uuid::from_u128(0xf0_01);
const AUDIT: Uuid = Uuid::from_u128(0xa0_01);
/// A migrated database and its DSN, which the test holds for its life (the directory goes with it).
async fn harness() -> (
    toolkit_db::DBProvider<toolkit_db::DbError>,
    crate::test_support::TestDsn,
) {
    let (db, _, _, dsn) = test_db().await;
    (db, dsn)
}
/// Build the fields every audit-row class shares, for the tests below.
fn common(
    audit_id: Uuid,
    tenant_id: Uuid,
    actor_ref: Uuid,
    action: &str,
    subject_kind: &str,
    written_at: OffsetDateTime,
) -> AuditCommon {
    AuditCommon {
        audit_id,
        tenant_id,
        actor_ref,
        action: action.to_owned(),
        subject_kind: subject_kind.to_owned(),
        reason: Some("test reason".to_owned()),
        correlation_id: None,
        written_at,
        lifecycle: LifecycleMove::NONE,
    }
}

/// Read one `products_audit_log` row by `audit_id`, for the tests below.
async fn find_audit_row(
    runner: &impl toolkit_db::secure::DBRunner,
    scope: &AccessScope,
    audit_id: Uuid,
) -> audit_log::Model {
    audit_log::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(audit_log::Column::AuditId.eq(audit_id)))
        .one(runner)
        .await
        .expect("read the audit row")
        .expect("the audit row exists")
}

/// An eventless-act row carries neither `error_code` — the refusal class's
/// column — nor `session_id` — the elevated-read class's — since this class
/// is neither.
#[tokio::test]
async fn an_eventless_act_row_carries_neither_error_code_nor_session_id() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);

    let actor_ref = crate::test_support::authed_ctx(TENANT).subject_id();

    write_eventless_act_audit(
        &conn,
        &scope,
        common(
            AUDIT,
            TENANT,
            actor_ref,
            "publish.scheduled",
            "product",
            at(10),
        ),
        PRODUCT,
        Some(2),
    )
    .await
    .expect("write eventless act audit row");

    let row = find_audit_row(&conn, &scope, AUDIT).await;

    assert_eq!(row.subject_id, Some(PRODUCT));
    assert_eq!(row.subject_revision, Some(2));
    assert_eq!(row.error_code, None);
    assert_eq!(row.session_id, None);
    assert_eq!(row.seal_state, "unsealed");
}

#[tokio::test]
async fn audit_rolls_back_with_the_act_and_foreign_scope_cannot_write_it() {
    use crate::infra::storage::repo::{insert_category, list_categories};
    let (db, scope, tenant, dsn) = test_db().await;
    let tx_scope = scope.clone();
    let actor = crate::test_support::authed_ctx(tenant).subject_id();
    let result = db
        .db()
        .transaction_with_retry::<(), toolkit_db::DbError, _, _>(
            toolkit_db::secure::TxConfig::default(),
            |_| None,
            move |tx| {
                let scope = tx_scope.clone();
                Box::pin(async move {
                    let c = insert_category(
                        tx,
                        &scope,
                        tenant,
                        crate::domain::category::NewCategory {
                            code: "rollback".into(),
                            name: "rollback".into(),
                            is_default: false,
                            sort_order: 0,
                        },
                        at(9),
                    )
                    .await
                    .map_err(|e| toolkit_db::DbError::Sea(e.to_db_err()))?;
                    write_eventless_act_audit(
                        tx,
                        &scope,
                        common(
                            Uuid::new_v4(),
                            tenant,
                            actor,
                            "category.create",
                            "category",
                            at(9),
                        ),
                        c.id,
                        Some(1),
                    )
                    .await
                    .map_err(|e| toolkit_db::DbError::Sea(e.to_db_err()))?;
                    Err(toolkit_db::DbError::InvalidParameter(
                        "rollback probe".into(),
                    ))
                })
            },
        )
        .await;
    assert!(result.is_err());
    assert!(
        list_categories(&db.conn().unwrap(), &scope, tenant)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        crate::test_support::raw_i64(&dsn, "SELECT COUNT(*) AS v FROM products_audit_log").await,
        0
    );
    let foreign = AccessScope::for_tenant(Uuid::new_v4());
    assert!(
        write_eventless_act_audit(
            &db.conn().unwrap(),
            &foreign,
            common(
                Uuid::new_v4(),
                tenant,
                actor,
                "category.create",
                "category",
                at(9)
            ),
            Uuid::new_v4(),
            Some(1)
        )
        .await
        .is_err()
    );
}

/// The batch writer stores every row as the one-row writer would, across the per-statement cap;
/// a row of another tenant refuses the whole batch before anything is written; nothing to write
/// writes nothing.
#[tokio::test]
async fn a_batch_of_audit_rows_is_written_whole_or_not_at_all() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);
    let count = || async {
        audit_log::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(audit_log::Column::TenantId.eq(TENANT)))
            .all(&conn)
            .await
            .unwrap()
            .len()
    };
    write_eventless_act_audits(&conn, TENANT, Vec::new())
        .await
        .unwrap();
    assert_eq!(count().await, 0);

    let foreign = Uuid::from_u128(0x7e_12);
    let stray = vec![
        (
            common(Uuid::now_v7(), TENANT, PRODUCT, "a", "sku", at(0)),
            PRODUCT,
            Some(1),
        ),
        (
            common(Uuid::now_v7(), foreign, PRODUCT, "b", "sku", at(0)),
            PRODUCT,
            Some(1),
        ),
    ];
    assert!(matches!(
        write_eventless_act_audits(&conn, TENANT, stray).await,
        Err(RepoError::Db(_))
    ));
    assert_eq!(count().await, 0, "a stray row refuses the whole batch");

    let n = AUDIT_ROWS_PER_INSERT + 1;
    let rows: Vec<_> = (0..n)
        .map(|_| {
            let mut c = common(
                Uuid::now_v7(),
                TENANT,
                PRODUCT,
                "sku.fence_expired",
                "sku",
                at(0),
            );
            c.lifecycle = LifecycleMove::between(Lifecycle::Deprecated, Lifecycle::Published);
            (c, PRODUCT, Some(7))
        })
        .collect();
    let last = rows[n - 1].0.audit_id;
    write_eventless_act_audits(&conn, TENANT, rows)
        .await
        .unwrap();
    assert_eq!(count().await, n);
    let row = find_audit_row(&conn, &scope, last).await;
    assert_eq!(
        (
            row.action.as_str(),
            row.subject_id,
            row.subject_revision,
            row.from_lifecycle.as_deref(),
            row.to_lifecycle.as_deref(),
            row.seal_state.as_str(),
        ),
        (
            "sku.fence_expired",
            Some(PRODUCT),
            Some(7),
            Some("deprecated"),
            Some("published"),
            "unsealed"
        )
    );
}
