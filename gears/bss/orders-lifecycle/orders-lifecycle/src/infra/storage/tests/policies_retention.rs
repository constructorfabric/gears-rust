use super::*;
use toolkit_security::AccessScope;
#[tokio::test]
async fn native_ttl_and_policy_promotions_preserve_permanent_defaults_and_generations()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (policy, raw) = pg.role("policy").await?;
    let (_, runtime) = pg.role("runtime").await?;
    let scope = AccessScope::for_resources(vec![u(0x181), u(0x121), u(500)]);
    let ttl = repo::private::find_state_ttl_policy(&policy.conn()?, &scope, u(0x181))
        .await?
        .unwrap();
    assert_eq!(
        super::super::interval::CalendarInterval::parse(ttl.ttl_duration.as_ref().unwrap())?.days,
        90
    );
    policy
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut promoted = ttl.clone();
                promoted.ttl_duration = Some("1 mon 2 days 00:00:00.000001".into());
                promoted.policy_revision += 1;
                promoted.provisional = false;
                repo::mutable::replace_state_ttl_policy(tx, &scope, &ttl, promoted).await?;
                anyhow::Ok(())
            })
        })
        .await?;
    let scope = AccessScope::for_resources(vec![u(0x181), u(0x121), u(500)]);
    let ttl = repo::private::find_state_ttl_policy(&policy.conn()?, &scope, u(0x181))
        .await?
        .unwrap();
    let parsed =
        super::super::interval::CalendarInterval::parse(ttl.ttl_duration.as_ref().unwrap())?;
    assert_eq!((parsed.months, parsed.days, parsed.microseconds), (1, 2, 1));
    assert!(runtime.execute_unprepared("UPDATE bss_orders__state_ttl_policy SET ttl_duration=interval '1 day',policy_revision=policy_revision+1").await.is_err());
    assert!(
        raw.execute_unprepared("DELETE FROM bss_orders__state_ttl_policy WHERE scope='platform'")
            .await
            .is_err()
    );
    assert!(
        raw.execute_unprepared(
            "DELETE FROM bss_orders__date_policy WHERE resource_tenant_id IS NULL"
        )
        .await
        .is_err()
    );
    for duration in ["interval '0'", "interval '-1 month'"] {
        assert!(raw.execute_unprepared(&format!("UPDATE bss_orders__state_ttl_policy SET ttl_duration={duration},policy_revision=policy_revision+1 WHERE state='draft'")).await.is_err());
    }
    // Non-production platform NULL is representable, never a code duration default.
    raw.execute_unprepared("UPDATE bss_orders__state_ttl_policy SET ttl_duration=NULL,policy_revision=policy_revision+1 WHERE state='draft'").await?;
    assert!(
        repo::private::find_state_ttl_policy(&policy.conn()?, &scope, u(0x181))
            .await?
            .unwrap()
            .ttl_duration
            .is_none()
    );
    raw.execute_unprepared(&format!("INSERT INTO bss_orders__state_ttl_policy VALUES('{}','seller','{}','draft',interval '1 month',false,1,'promotion',now())",u(500),u(20))).await?;
    assert_eq!(pg.scalar("SELECT policy_revision AS n FROM bss_orders__state_ttl_policy WHERE state='draft' AND scope='platform'").await?,4);
    raw.execute_unprepared("DELETE FROM bss_orders__state_ttl_policy WHERE scope='seller'")
        .await?;
    assert_eq!(pg.scalar("SELECT policy_revision AS n FROM bss_orders__state_ttl_policy WHERE state='draft' AND scope='platform'").await?,5);
    raw.execute_unprepared(&format!(
        "INSERT INTO bss_orders__date_policy VALUES('{}','{}',true,false,2,now())",
        u(500),
        u(10)
    ))
    .await?;
    raw.execute_unprepared(
        "DELETE FROM bss_orders__date_policy WHERE resource_tenant_id IS NOT NULL",
    )
    .await?;
    assert!(
        raw.execute_unprepared(&format!(
            "INSERT INTO bss_orders__date_policy VALUES('{}','{}',true,false,2,now())",
            u(501),
            u(10)
        ))
        .await
        .is_err()
    );
    raw.execute_unprepared(&format!(
        "INSERT INTO bss_orders__date_policy VALUES('{}','{}',true,false,5,now())",
        u(501),
        u(10)
    ))
    .await?;
    Ok(())
}
#[tokio::test]
async fn only_expired_preview_and_read_logs_are_retained_for_deletion() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (_, retention) = pg.role("retention").await?;
    let (_, runtime) = pg.role("runtime").await?;
    create(&pg.db, 1).await?;
    for (id, order, age) in [
        (1, "NULL".to_owned(), 8),
        (2, "NULL".to_owned(), 0),
        (3, format!("'{}'", u(1)), 100),
    ] {
        pg.sql(&format!("INSERT INTO bss_orders__gate_outcome(outcome_id,run_id,subject_tenant_id,subject_id,resource_tenant_id,seller_tenant_id,payer_tenant_id,order_id,predicate,has_catalog_selection,verdict,mapping_version,applicability,producer_results,evaluated_at) VALUES('{}','{}','{}','{}','{}','{}','{}',{order},'test',false,'passed','v1','required','[]',now()-interval '{age} days')",u(id),u(id+10),u(10),u(40),u(10),u(20),u(30))).await?;
    }
    assert!(
        runtime
            .execute_unprepared("DELETE FROM bss_orders__gate_outcome")
            .await
            .is_err()
    );
    for id in [2, 3] {
        assert!(
            retention
                .execute_unprepared(&format!(
                    "DELETE FROM bss_orders__gate_outcome WHERE outcome_id='{}'",
                    u(id)
                ))
                .await
                .is_err()
        );
    }
    retention
        .execute_unprepared(&format!(
            "DELETE FROM bss_orders__gate_outcome WHERE outcome_id='{}'",
            u(1)
        ))
        .await?;
    for (id, age) in [(1, 91), (2, 0)] {
        pg.sql(&format!("INSERT INTO bss_orders__read_access_log VALUES('{}',NULL,NULL,'{}','user','list','refused','not-permitted',NULL,NULL,now()-interval '{age} days')",u(id),u(40))).await?;
    }
    assert!(
        retention
            .execute_unprepared(&format!(
                "DELETE FROM bss_orders__read_access_log WHERE access_id='{}'",
                u(2)
            ))
            .await
            .is_err()
    );
    retention
        .execute_unprepared(&format!(
            "DELETE FROM bss_orders__read_access_log WHERE access_id='{}'",
            u(1)
        ))
        .await?;
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__gate_outcome")
            .await?,
        2
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__read_access_log")
            .await?,
        1
    );
    // A committed old row stays forever even though the retention role has DELETE privilege.
    let mut audit = super::constraints::audit(101);
    audit.trigger = "create".into();
    audit.reason = "create".into();
    audit.outcome = "committed".into();
    audit.order_id = Some(u(1));
    audit.audit_tenant_id = Some(u(10));
    audit.resource_tenant_id = Some(u(10));
    audit.sequence = Some(1);
    audit.prev_hash = Some(vec![0; 32]);
    audit.to_state = Some("draft".into());
    audit.version = Some(1);
    audit.created_at = time::OffsetDateTime::now_utc() - time::Duration::days(100);
    super::constraints::write(&pg, audit).await?;
    assert!(
        retention
            .execute_unprepared("DELETE FROM bss_orders__transition_audit")
            .await
            .is_err()
    );
    Ok(())
}
