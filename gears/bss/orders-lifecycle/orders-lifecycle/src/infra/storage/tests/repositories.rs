use super::*;
use crate::infra::storage::interval::CalendarInterval;
use toolkit_security::{
    AccessScope,
    access_scope::{ScopeConstraint, ScopeFilter},
};
fn scoped(filters: Vec<ScopeFilter>) -> AccessScope {
    AccessScope::from_constraints(vec![ScopeConstraint::new(filters)])
}
fn payer(id: u128) -> AccessScope {
    scoped(vec![ScopeFilter::eq("payer_tenant_id", u(id))])
}
fn draft(id: u128, line: u128) -> entity::draft_content::Model {
    entity::draft_content::Model {
        order_id: u(id),
        line_id: u(line),
        plan_id: u(200),
        plan_revision_id: u(201),
        selected_items: serde_json::json!([]),
        currency: "USD".into(),
        contract_effective_date: None,
        service_activation_date: None,
        acceptance_due_date: None,
        term_duration: Some("1 year 2 mons 3 days 04:05:06.123456".into()),
        term_kind: "finite".into(),
        authored_term: serde_json::json!({"kind":"calendar","years":1,"months":2,"days":3,"microseconds":14_706_123_456i64}),
        billing_cycle: Some("month".into()),
    }
}
#[tokio::test]
async fn current_parent_scope_controls_sparse_history_and_child_inserts() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    db.transaction_ref_mapped(|tx| {
        Box::pin(async move {
            let scope = payer(30);
            let observed = repo::find_order(tx, &scope, u(1)).await?.unwrap();
            let mut lock = repo::LockedOrder::acquire(tx, &scope, &observed).await?;
            let mut new = observed.clone();
            new.version_allocation_high_water = 4;
            lock.replace(&scope, new.clone()).await?;
            lock.insert_order_version(version(1, 4, Some(1))).await?;
            new.current_version = 4;
            new.payer_tenant_id = u(31);
            lock.replace(&payer(31), new).await?;
            // Parent lock cannot insert a child for another (even existent) order.
            assert!(
                lock.insert_order_line_identity(entity::order_line_identity::Model {
                    order_id: u(2),
                    line_id: u(11),
                    created_at: now()
                })
                .await
                .is_err()
            );
            anyhow::Ok(())
        })
    })
    .await?;
    assert!(
        repo::children::versions(&db.conn()?, &payer(30), u(1))
            .await?
            .is_empty()
    );
    let found = repo::children::versions(&db.conn()?, &payer(31), u(1)).await?;
    assert_eq!(
        found.iter().map(|r| r.version).collect::<Vec<_>>(),
        vec![1, 4]
    );
    assert_eq!(found[1].supersedes_version, Some(1));
    assert_eq!(found[0].payer_tenant_id, u(30));
    assert!(
        repo::children::version(&db.conn()?, &payer(31), u(1), 2)
            .await?
            .is_none()
    );
    assert!(
        repo::children::version(&db.conn()?, &payer(31), u(1), 3)
            .await?
            .is_none()
    );
    let denied: anyhow::Result<()> = db
        .transaction_ref_mapped(|tx| {
            Box::pin(async move {
                repo::LockedOrder::acquire(tx, &payer(30), &order(1)).await?;
                Ok(())
            })
        })
        .await;
    assert!(denied.is_err());
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order_line_identity")
            .await?,
        0
    );
    Ok(())
}
#[tokio::test]
async fn complete_proposed_insert_validates_all_axes_and_or_paths() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    let wrong = scoped(vec![
        ScopeFilter::eq("id", u(1)),
        ScopeFilter::eq("resource_tenant_id", u(10)),
        ScopeFilter::eq("seller_tenant_id", u(20)),
        ScopeFilter::eq("payer_tenant_id", u(999)),
    ]);
    let failed: anyhow::Result<()> = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                repo::insert_order(tx, &wrong, order(1)).await?;
                Ok(())
            })
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order")
            .await?,
        0
    );
    let branches = AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![
            ScopeFilter::eq("resource_tenant_id", u(10)),
            ScopeFilter::eq("seller_tenant_id", u(21)),
        ]),
        ScopeConstraint::new(vec![
            ScopeFilter::eq("resource_tenant_id", u(11)),
            ScopeFilter::eq("seller_tenant_id", u(20)),
        ]),
    ]);
    assert!(repo::validate_order(&order(1), &branches).is_err());
    assert!(repo::validate_order(&order(1), &AccessScope::deny_all()).is_err());
    create(&db, 1).await?;
    let failed: anyhow::Result<()> = db
        .transaction_ref_mapped(|tx| {
            Box::pin(async move {
                let scope = payer(30);
                let observed = repo::find_order(tx, &scope, u(1)).await?.unwrap();
                let mut locked = repo::LockedOrder::acquire(tx, &scope, &observed).await?;
                let mut wrong = observed;
                wrong.payer_tenant_id = u(999);
                locked.replace(&scope, wrong).await?;
                Ok(())
            })
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        repo::find_order(&db.conn()?, &payer(30), u(1))
            .await?
            .unwrap()
            .payer_tenant_id,
        u(30)
    );
    Ok(())
}
#[tokio::test]
async fn native_interval_scoped_insert_update_read_and_tagged_intent() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    db.transaction_ref_mapped(|tx|Box::pin(async move {
        let scope=payer(30);let row=repo::find_order(tx,&scope,u(1)).await?.unwrap();let lock=repo::LockedOrder::acquire(tx,&scope,&row).await?;
                let inserted=lock.add_draft_line(draft(1,101), now()).await?;

        let interval=CalendarInterval::parse(inserted.term_duration.as_ref().unwrap())?;
        assert_eq!(interval,CalendarInterval{months:14,days:3,microseconds:14_706_123_456});
        assert_eq!(CalendarInterval::parse(&interval.postgres())?,interval);
        let mut rolling=inserted.clone();rolling.term_kind="rolling".into();rolling.term_duration=None;rolling.authored_term=serde_json::json!({"kind":"rolling"});
        lock.replace_draft_content(&inserted,rolling.clone()).await?;
        let mut missing=rolling.clone();missing.term_kind="missing".into();missing.authored_term=serde_json::json!({"kind":"missing"});
        lock.replace_draft_content(&rolling,missing.clone()).await?;
        let mut finite=missing.clone();finite.term_kind="finite".into();finite.term_duration=Some("2 years 0.000001 seconds".into());finite.authored_term=serde_json::json!({"kind":"calendar","years":2,"months":0,"days":0,"microseconds":1});
        lock.replace_draft_content(&missing,finite).await?;
        anyhow::Ok(())
    })).await?;
    let row = repo::children::draft_content_for_order(&db.conn()?, &payer(30), u(1))
        .await?
        .remove(0);
    assert_eq!(
        super::super::interval::CalendarInterval::parse(row.term_duration.as_ref().unwrap())?,
        super::super::interval::CalendarInterval {
            months: 24,
            days: 0,
            microseconds: 1
        }
    );
    for sql in [
        "UPDATE bss_orders__draft_content SET term_kind='rolling'",
        "UPDATE bss_orders__draft_content SET term_duration=NULL",
        "UPDATE bss_orders__draft_content SET term_duration=interval '0'",
        "UPDATE bss_orders__draft_content SET term_duration=interval '-1 year'",
        "UPDATE bss_orders__draft_content SET term_duration=interval '999999999999 years'",
        "UPDATE bss_orders__draft_content SET term_duration=interval '730 days 0.000001 seconds'",
    ] {
        assert!(pg.sql(sql).await.is_err(), "{sql}");
    }
    assert!(super::super::interval::CalendarInterval::parse("999999999999 years").is_err());
    assert!(super::super::interval::CalendarInterval::parse("00:00:00.0000001").is_err());
    assert!(super::super::interval::CalendarInterval::parse("2562047789:00:00").is_err());
    Ok(())
}
#[tokio::test]
async fn draft_removal_preserves_reserved_identity_and_rejects_reuse() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    db.transaction_ref_mapped(|tx| {
        Box::pin(async move {
            let scope = payer(30);
            let row = repo::find_order(tx, &scope, u(1)).await?.unwrap();
            let lock = repo::LockedOrder::acquire(tx, &scope, &row).await?;
            lock.add_draft_line(draft(1, 101), now()).await?;
            assert!(lock.remove_draft_line(u(101)).await?);
            anyhow::Ok(())
        })
    })
    .await?;
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__draft_content")
            .await?,
        0
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order_line_identity")
            .await?,
        1
    );
    let reused: anyhow::Result<()> = db
        .transaction_ref_mapped(|tx| {
            Box::pin(async move {
                let scope = payer(30);
                let row = repo::find_order(tx, &scope, u(1)).await?.unwrap();
                repo::LockedOrder::acquire(tx, &scope, &row)
                    .await?
                    .add_draft_line(draft(1, 101), now())
                    .await?;
                Ok(())
            })
        })
        .await;
    assert!(reused.is_err());
    assert!(pg.sql("INSERT INTO bss_orders__order_line_identity SELECT * FROM bss_orders__order_line_identity").await.is_err());
    assert!(
        pg.sql("DELETE FROM bss_orders__order_line_identity")
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn interval_codec_preserves_both_postgres_signed_bounds() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    for micros in [i64::MIN, i64::MAX, -1, 0, 1] {
        let row = pg
            .raw
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT $1::interval::text AS value",
                [format!("{micros} microseconds").into()],
            ))
            .await?
            .unwrap();
        let text: String = row.try_get("", "value")?;
        assert_eq!(
            CalendarInterval::parse(&text)?,
            CalendarInterval {
                months: 0,
                days: 0,
                microseconds: micros
            }
        );
    }
    assert!(CalendarInterval::parse("2562047788:00:54.775808").is_err());
    assert!(CalendarInterval::parse("-2562047788:00:54.775809").is_err());
    Ok(())
}

#[tokio::test]
async fn scoped_reads_work_in_read_only_transactions_and_stale_parent_is_rejected()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    let observed = db
        .transaction_ref_mapped_with_config(toolkit_db::secure::TxConfig::read_only(), |tx| {
            Box::pin(async move {
                let row = repo::find_order(tx, &payer(30), u(1)).await?.unwrap();
                assert_eq!(
                    repo::children::versions(tx, &payer(30), u(1)).await?.len(),
                    1
                );
                anyhow::Ok(row)
            })
        })
        .await?;
    db.transaction_ref_mapped(|tx| {
        Box::pin(async move {
            let scope = payer(30);
            let row = repo::find_order(tx, &scope, u(1)).await?.unwrap();
            let mut lock = repo::LockedOrder::acquire(tx, &scope, &row).await?;
            let mut proposed = row;
            proposed.payer_tenant_id = u(31);
            lock.replace(&payer(31), proposed).await?;
            anyhow::Ok(())
        })
    })
    .await?;
    let stale: anyhow::Result<()> = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                repo::LockedOrder::acquire(tx, &payer(31), &observed).await?;
                Ok(())
            })
        })
        .await;
    assert!(stale.is_err());
    assert!(
        repo::children::versions(&db.conn()?, &payer(30), u(1))
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn resolved_audit_pages_by_keyset_without_truncation_or_foreign_rows() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    create(&db, 2).await?;
    // Two rows share an instant, so the audit_id tie-breaker must order and resume them.
    let instants = [0, 1, 1, 2, 3];
    for (n, seconds) in instants.iter().enumerate() {
        let mut row = super::constraints::audit(500 + n as u128);
        row.order_id = Some(u(1));
        row.audit_tenant_id = Some(u(10));
        row.resource_tenant_id = Some(u(10));
        row.from_state = Some("draft".into());
        row.to_state = row.from_state.clone();
        row.version = Some(1);
        row.created_at = now() + time::Duration::seconds(*seconds);
        super::constraints::write(&pg, row).await?;
    }
    let mut foreign = super::constraints::audit(600);
    foreign.order_id = Some(u(2));
    foreign.requested_order_ref = Some(u(2));
    foreign.audit_tenant_id = Some(u(10));
    foreign.resource_tenant_id = Some(u(10));
    foreign.from_state = Some("draft".into());
    foreign.to_state = foreign.from_state.clone();
    foreign.version = Some(1);
    super::constraints::write(&pg, foreign).await?;
    let conn = db.conn()?;
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = repo::private::resolved_audit_page(&conn, &payer(30), u(1), after, 2).await?;
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 2);
        let last = page.last().unwrap();
        after = Some((last.created_at, last.audit_id));
        seen.extend(page.into_iter().map(|r| r.audit_id));
    }
    assert_eq!(seen, (500..505).map(u).collect::<Vec<_>>());
    // The current parent scope governs disclosure; an unrelated payer sees no audit rows.
    assert!(
        repo::private::resolved_audit_page(&conn, &payer(99), u(1), None, 200)
            .await?
            .is_empty()
    );
    for limit in [0, repo::private::MAX_AUDIT_PAGE + 1] {
        assert!(
            repo::private::resolved_audit_page(&conn, &payer(30), u(1), None, limit)
                .await
                .is_err()
        );
    }
    Ok(())
}

#[tokio::test]
async fn interval_overflow_and_mismatched_intent_fail_through_the_secure_update_path()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    db.transaction_ref_mapped(|tx| {
        Box::pin(async move {
            let scope = payer(30);
            let row = repo::find_order(tx, &scope, u(1)).await?.unwrap();
            repo::LockedOrder::acquire(tx, &scope, &row)
                .await?
                .add_draft_line(draft(1, 101), now())
                .await?;
            anyhow::Ok(())
        })
    })
    .await?;
    let stored = repo::children::draft_content_for_order(&db.conn()?, &payer(30), u(1))
        .await?
        .remove(0);
    let mut overflow = stored.clone();
    overflow.term_duration = Some("999999999999 years".into());
    let mut mismatched = stored.clone();
    // 730 days is not the authored two calendar years; interval '=' would call them equal.
    mismatched.term_duration = Some("730 days".into());
    mismatched.authored_term =
        serde_json::json!({"kind":"calendar","years":2,"months":0,"days":0,"microseconds":0});
    let mut untagged_null = stored.clone();
    untagged_null.term_duration = None;
    for proposed in [overflow, mismatched, untagged_null] {
        let expected = stored.clone();
        let result: anyhow::Result<()> = db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let scope = payer(30);
                    let row = repo::find_order(tx, &scope, u(1)).await?.unwrap();
                    repo::LockedOrder::acquire(tx, &scope, &row)
                        .await?
                        .replace_draft_content(&expected, proposed)
                        .await?;
                    Ok(())
                })
            })
            .await;
        assert!(result.is_err());
    }
    let unchanged = repo::children::draft_content_for_order(&db.conn()?, &payer(30), u(1))
        .await?
        .remove(0);
    assert_eq!(unchanged, stored);
    Ok(())
}
