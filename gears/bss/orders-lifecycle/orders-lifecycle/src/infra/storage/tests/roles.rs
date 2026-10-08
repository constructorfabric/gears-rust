use super::*;
use sea_orm::{EntityTrait, IntoActiveModel};
use toolkit_security::AccessScope;
#[tokio::test]
async fn every_table_has_expected_role_isolation_and_append_only_defenses() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    create(&pg.db, 1).await?;
    let inv: serde_json::Value =
        serde_json::from_str(include_str!("../migrations/schema-inventory.json"))?;
    let append: Vec<_> = inv["tables"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| {
            ["append", "audit", "diagnostic", "read_log", "checkpoint"]
                .contains(&t["mutation"].as_str().unwrap())
        })
        .collect();
    for role in [
        "runtime",
        "business",
        "private",
        "discovery",
        "maintenance",
        "verifier",
        "checkpoint",
        "retention",
        "policy",
    ] {
        let (_, raw) = pg.role(role).await?;
        for t in &append {
            let name = t["physical"].as_str().unwrap();
            let pk = t["pk"][0].as_str().unwrap();
            assert!(
                raw.execute_unprepared(&format!("UPDATE {name} SET {pk}={pk}"))
                    .await
                    .is_err(),
                "{role} UPDATE {name}"
            );
            if role != "retention"
                || !["audit", "diagnostic", "read_log"].contains(&t["mutation"].as_str().unwrap())
            {
                assert!(
                    raw.execute_unprepared(&format!("DELETE FROM {name}"))
                        .await
                        .is_err(),
                    "{role} DELETE {name}"
                );
            }
        }
        if role != "runtime" && role != "business" {
            assert!(
                raw.execute_unprepared(
                    "UPDATE bss_orders__order SET draft_revision=draft_revision+1"
                )
                .await
                .is_err(),
                "{role} must not mutate order"
            );
        }
        if ["business", "discovery", "retention", "policy"].contains(&role) {
            assert!(
                raw.execute_unprepared("SELECT * FROM bss_orders__audit_checkpoint")
                    .await
                    .is_err()
            );
        }
        if [
            "business",
            "discovery",
            "checkpoint",
            "verifier",
            "retention",
            "policy",
        ]
        .contains(&role)
        {
            assert!(raw.execute_unprepared("INSERT INTO event_broker_producer_registrations VALUES('unauthorized',gen_random_uuid(),'chained','test',0,now(),now())").await.is_err());
        }
    }
    // Privileged migration owner proves the trigger (not only lack of privileges) rejects evidence rewrites.
    for sql in [
        "UPDATE bss_orders__order_version SET actor=actor",
        "DELETE FROM bss_orders__order_version",
        "DELETE FROM bss_orders__order",
    ] {
        assert!(pg.sql(sql).await.is_err(), "{sql}");
    }
    Ok(())
}
#[tokio::test]
async fn checkpoint_writer_is_separate_and_partial_snapshot_rolls_back() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (writer, _) = pg.role("checkpoint").await?;
    let (runtime, _) = pg.role("runtime").await?;
    let header = entity::audit_checkpoint::Model {
        audit_tenant_id: u(10),
        checkpoint_sequence: 1,
        format_version: 1,
        captured_at: now(),
        member_count: 1,
        prev_checkpoint_hash: vec![0; 32],
        checkpoint_hash: vec![1; 32],
    };
    // The writer accepts only a TargetScope from a discovered audit namespace (D-184).
    create(&pg.db, 1).await?;
    let mut evidence = super::constraints::audit(101);
    evidence.trigger = "create".into();
    evidence.reason = "create".into();
    evidence.outcome = "committed".into();
    evidence.order_id = Some(u(1));
    evidence.audit_tenant_id = Some(u(10));
    evidence.resource_tenant_id = Some(u(10));
    evidence.sequence = Some(1);
    evidence.prev_hash = Some(vec![0; 32]);
    evidence.to_state = Some("draft".into());
    evidence.version = Some(1);
    super::constraints::write(&pg, evidence).await?;
    // (The verifier role is provisioned later in this test; discovery here uses the owner.)
    let scope = super::discovered_namespace(&pg.db, u(10)).await?;
    // Runtime has no checkpoint grant; the writer cannot commit a header without its members.
    for db in [&runtime, &writer] {
        let (scope, header) = (scope.clone(), header.clone());
        let result: anyhow::Result<()> = db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    repo::private::insert_audit_checkpoint(tx, &scope, header).await?;
                    Ok(())
                })
            })
            .await;
        assert!(result.is_err());
    }
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__audit_checkpoint")
            .await?,
        0
    );
    writer
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                repo::private::insert_audit_checkpoint(tx, &scope, header).await?;
                // No FK to a live order: the expected chain remains evidence even if live data is lost.
                repo::private::insert_audit_checkpoint_member(
                    tx,
                    &scope,
                    entity::audit_checkpoint_member::Model {
                        audit_tenant_id: u(10),
                        checkpoint_sequence: 1,
                        order_id: u(999),
                        audit_sequence: 1,
                        entry_hash: vec![1; 32],
                    },
                )
                .await?;
                anyhow::Ok(())
            })
        })
        .await?;
    for table in ["audit_checkpoint", "audit_checkpoint_member"] {
        assert!(
            pg.sql(&format!(
                "UPDATE bss_orders__{table} SET audit_tenant_id=audit_tenant_id"
            ))
            .await
            .is_err()
        );
        assert!(
            pg.sql(&format!("DELETE FROM bss_orders__{table}"))
                .await
                .is_err()
        );
    }
    let (_, verifier) = pg.role("verifier").await?;
    verifier
        .execute_unprepared("SELECT * FROM bss_orders__audit_checkpoint_member")
        .await?;
    assert!(verifier.execute_unprepared("INSERT INTO bss_orders__audit_checkpoint SELECT * FROM bss_orders__audit_checkpoint").await.is_err());
    Ok(())
}
#[tokio::test]
async fn refusal_retention_preserves_long_lived_replay_and_rejects_other_deletes()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (_, retention) = pg.role("retention").await?;
    let (_, runtime) = pg.role("runtime").await?;
    let mut expired = super::constraints::audit(101);
    expired.created_at = time::OffsetDateTime::now_utc() - time::Duration::days(91);
    let mut recent = expired.clone();
    recent.audit_id = u(102);
    recent.created_at = time::OffsetDateTime::now_utc();
    super::constraints::write(&pg, expired).await?;
    super::constraints::write(&pg, recent).await?;
    pg.sql(&format!("INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,request_fingerprint,status,execution_id,fencing_generation,outcome,outcome_reason,audit_id,settled_response,created_at,expires_at) VALUES('create','actor','long','hash','settled','{}',0,'refused','order-not-found','{}','{{\"formatVersion\":1,\"body\":{{\"reason\":\"order-not-found\"}}}}',now()-interval '91 days',now()+interval '30 days')",u(201),u(101))).await?;
    assert!(
        runtime
            .execute_unprepared(&format!(
                "DELETE FROM bss_orders__transition_audit WHERE audit_id='{}'",
                u(101)
            ))
            .await
            .is_err()
    );
    assert!(
        retention
            .execute_unprepared(&format!(
                "DELETE FROM bss_orders__transition_audit WHERE audit_id='{}'",
                u(102)
            ))
            .await
            .is_err()
    );
    retention
        .execute_unprepared(&format!(
            "DELETE FROM bss_orders__transition_audit WHERE audit_id='{}'",
            u(101)
        ))
        .await?;
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM bss_orders__idempotency WHERE settled_response->>'formatVersion'='1'").await?,1);
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__transition_audit")
            .await?,
        1
    );
    assert!(
        retention
            .execute_unprepared("DELETE FROM bss_orders__idempotency")
            .await
            .is_err()
    );
    assert!(
        runtime
            .execute_unprepared("DELETE FROM bss_orders__idempotency")
            .await
            .is_err()
    );
    for body in [
        "{}",
        "null",
        "{\"formatVersion\":null}",
        "{\"formatVersion\":2}",
        "{\"formatVersion\":\"1\"}",
    ] {
        let sql = format!(
            "INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,request_fingerprint,status,execution_id,fencing_generation,outcome,outcome_reason,audit_id,settled_response,created_at,expires_at) VALUES('create','actor','bad','hash','settled',gen_random_uuid(),0,'refused','order-not-found','{}','{body}',now(),now()+interval '1 day')",
            u(102)
        );
        assert!(pg.sql(&sql).await.is_err(), "{body}");
    }
    Ok(())
}
fn committed(id: u128, trigger: &str, sequence: i64) -> entity::transition_audit::Model {
    let mut a = super::constraints::audit(id);
    a.order_id = Some(u(1));
    a.audit_tenant_id = Some(u(10));
    a.resource_tenant_id = Some(u(10));
    a.sequence = Some(sequence);
    a.prev_hash = Some(vec![0; 32]);
    a.from_state = Some("in_fulfillment".into());
    a.to_state = a.from_state.clone();
    a.trigger = trigger.into();
    a.reason = trigger.into();
    a.outcome = "committed".into();
    a.version = Some(1);
    a
}
#[tokio::test]
async fn grants_require_current_source_and_deferred_committed_writer_audit() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    create(&runtime, 1).await?;
    let grant = entity::fulfillment_grant::Model {
        grant_id: u(201),
        order_id: u(1),
        order_version: 1,
        fulfillment_attempt_id: u(301),
        generation: 1,
        roster_receipt_digest: "verified".into(),
        roster: serde_json::json!([]),
        authority_fact_digest: "verified".into(),
        predecessor_grant_id: None,
        created_at: now(),
        execution_id: u(401),
        audit_id: u(101),
        audit_outcome: "committed".into(),
    };
    let initial = grant.clone();
    runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let scope = AccessScope::for_resources(vec![u(1)]);
                let row = repo::find_order(tx, &scope, u(1)).await?.unwrap();
                let mut lock = repo::LockedOrder::acquire(tx, &scope, &row).await?;
                let mut next = row;
                next.state = "in_fulfillment".into();
                next.spawn_signal_at = Some(now());
                next.fulfillment_control_generation = 1;
                lock.replace(&scope, next).await?;
                lock.set_audit_sequence_for_raw_fixture(2).await?;
                lock.insert_fulfillment_grant(initial).await?; // audit does not exist yet: FK must be deferred
                repo::private::insert_transition_audit(
                    tx,
                    &scope,
                    committed(101, "report-spawn-signal", 2),
                )
                .await?;
                anyhow::Ok(())
            })
        })
        .await?;
    let mut successor = grant.clone();
    successor.grant_id = u(202);
    successor.audit_id = u(102);
    successor.generation = 2;
    successor.predecessor_grant_id = Some(grant.grant_id);
    runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let scope = AccessScope::for_resources(vec![u(1)]);
                let row = repo::find_order(tx, &scope, u(1)).await?.unwrap();
                let mut lock = repo::LockedOrder::acquire(tx, &scope, &row).await?;
                let mut next = row;
                next.fulfillment_control_generation = 2;
                lock.replace(&scope, next).await?;
                lock.set_audit_sequence_for_raw_fixture(3).await?;
                lock.insert_fulfillment_grant(successor).await?;
                repo::private::insert_transition_audit(
                    tx,
                    &scope,
                    committed(102, "replace-fulfillment-grant", 3),
                )
                .await?;
                anyhow::Ok(())
            })
        })
        .await?;
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__fulfillment_grant")
            .await?,
        2
    );
    for (audit_id, token) in [(103, "draft-mutate"), (104, "resume")] {
        if token == "draft-mutate" {
            super::constraints::write(&pg, committed(audit_id, token, 4)).await?;
        }
        let mut bad = grant.clone();
        bad.grant_id = Uuid::new_v4();
        bad.audit_id = u(audit_id);
        bad.generation = 3;
        bad.predecessor_grant_id = Some(u(202));
        let result: anyhow::Result<()> = runtime
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::for_resources(vec![u(1)]);
                    let row = repo::find_order(tx, &scope, u(1)).await?.unwrap();
                    let mut lock = repo::LockedOrder::acquire(tx, &scope, &row).await?;
                    let mut next = row;
                    next.fulfillment_control_generation = 3;
                    lock.replace(&scope, next).await?;
                    lock.insert_fulfillment_grant(bad).await?;
                    Ok(())
                })
            })
            .await;
        assert!(result.is_err());
    }
    assert_eq!(
        pg.scalar("SELECT fulfillment_control_generation AS n FROM bss_orders__order")
            .await?,
        2
    );
    let mut refused = super::constraints::audit(105);
    refused.order_id = Some(u(1));
    refused.audit_tenant_id = Some(u(10));
    refused.resource_tenant_id = Some(u(10));
    refused.from_state = Some("in_fulfillment".into());
    refused.to_state = refused.from_state.clone();
    refused.version = Some(1);
    super::constraints::write(&pg, refused).await?;
    let mut bad = grant.clone();
    bad.grant_id = u(299);
    bad.audit_id = u(105);
    bad.generation = 3;
    assert!(
        entity::fulfillment_grant::Entity::insert(bad.into_active_model())
            .exec(&pg.raw)
            .await
            .is_err()
    );
    // D-198/D-201: a successor replaces only the current head (no fork or skipped grant), and
    // the first-spawn grant is issued once. The last case proves the head still advances.
    for (audit_id, token, predecessor, accepted) in [
        (106, "replace-fulfillment-grant", Some(u(201)), false),
        (107, "report-spawn-signal", None, false),
        (108, "replace-fulfillment-grant", Some(u(202)), true),
    ] {
        super::constraints::write(
            &pg,
            committed(audit_id, token, i64::try_from(audit_id)? - 101),
        )
        .await?;
        let mut next_grant = grant.clone();
        next_grant.grant_id = u(audit_id + 200);
        next_grant.audit_id = u(audit_id);
        next_grant.generation = 3;
        next_grant.predecessor_grant_id = predecessor;
        let result: anyhow::Result<()> = runtime
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::for_resources(vec![u(1)]);
                    let row = repo::find_order(tx, &scope, u(1)).await?.unwrap();
                    let mut lock = repo::LockedOrder::acquire(tx, &scope, &row).await?;
                    let mut next = row;
                    next.fulfillment_control_generation = 3;
                    lock.replace(&scope, next).await?;
                    lock.insert_fulfillment_grant(next_grant).await?;
                    Ok(())
                })
            })
            .await;
        if accepted {
            result?;
        } else {
            let error = format!("{:#}", result.unwrap_err());
            let expected = if predecessor.is_some() {
                "not the current grant"
            } else {
                "first dispatch grant already issued"
            };
            assert!(error.contains(expected), "{token}: {error}");
        }
    }
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__fulfillment_grant")
            .await?,
        3
    );
    Ok(())
}
