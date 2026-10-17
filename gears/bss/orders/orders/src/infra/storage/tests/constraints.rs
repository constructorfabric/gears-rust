use super::*;
use sea_orm::{EntityTrait, IntoActiveModel};
use toolkit_security::AccessScope;
use toolkit_security::access_scope::{ScopeConstraint, ScopeFilter};
/// A valid S2-10 `date_policy_switch_state` (the migration-seeded platform default).
const PLATFORM_SNAPSHOT: &str = r#"{"service_activation_required":false,"acceptance_due_required":false,"scope":"platform_default","resource_tenant_id":null,"policy_id":"00000000-0000-0000-0000-000000000121","revision":1}"#;
pub(super) fn audit(id: u128) -> entity::transition_audit::Model {
    entity::transition_audit::Model {
        audit_id: u(id),
        hash_version: 3,
        audit_tenant_id: None,
        subject_tenant_id: u(10),
        resource_tenant_id: None,
        order_id: None,
        requested_order_ref: Some(u(1)),
        sequence: None,
        prev_hash: None,
        entry_hash: vec![1; 32],
        from_state: None,
        to_state: None,
        trigger: "cancel".into(),
        outcome: "refused".into(),
        actor: u(40).to_string(),
        actor_class: "user".into(),
        delegation_proof_ref: None,
        reason: "order-not-found".into(),
        caller_reason: None,
        force_request_observation: None,
        changed_field: None,
        prior_value: None,
        new_value: None,
        idempotency_key: "test".into(),
        correlation_id: None,
        version: None,
        created_at: now(),
    }
}
pub(super) async fn write(pg: &Pg, row: entity::transition_audit::Model) -> anyhow::Result<()> {
    entity::transition_audit::Entity::insert(row.into_active_model())
        .exec(&pg.raw)
        .await?;
    Ok(())
}
#[tokio::test]
async fn audit_v3_closed_observation_and_frozen_v1_v2_readability() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    create(&pg.db, 1).await?;
    for (id, hash_version) in [(100, 1), (101, 2), (102, 3)] {
        let mut r = audit(id);
        r.hash_version = hash_version;
        write(&pg, r).await?;
    }
    assert_eq!(
        pg.scalar("SELECT count(DISTINCT hash_version) AS n FROM bss_orders__transition_audit")
            .await?,
        3
    );
    let (private, _) = pg.role("private").await?;
    let scope = AccessScope::for_tenant(u(10));
    // Custom subject property, not owner_tenant_id, is the private unresolved scope.

    let subject = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![ScopeFilter::eq(
        "subject_tenant_id",
        u(10),
    )])]);
    for (id, hash_version) in [(100, 1), (101, 2), (102, 3)] {
        let stored = repo::private::find_transition_audit(&private.conn()?, &subject, u(id))
            .await?
            .unwrap();
        assert_eq!(stored.hash_version, hash_version);
        assert!(stored.force_request_observation.is_none());
    }
    assert!(audit_tx(&private, &scope, audit(103)).await.is_err());
    let mut old = audit(104);
    old.hash_version = 2;
    assert!(audit_tx(&private, &subject, old).await.is_err());
    audit_tx(&private, &subject, audit(105)).await?;
    let mut request = audit(110);
    request.order_id = Some(u(1));
    request.audit_tenant_id = Some(u(10));
    request.resource_tenant_id = Some(u(10));
    request.trigger = "force-fail-unreconciled".into();
    request.reason = "second-approver-required".into();
    request.from_state = Some("in_fulfillment".into());
    request.to_state = request.from_state.clone();
    request.version = Some(1);
    request.force_request_observation =
        Some(serde_json::json!({"audit_sequence":5,"state":"in_fulfillment","version":1}));
    write(&pg, request.clone()).await?;
    let bad = vec![
        serde_json::json!({}),
        serde_json::json!(null),
        serde_json::json!({"audit_sequence":null,"state":"in_fulfillment","version":1}),
        serde_json::json!({"audit_sequence":1,"state":"in_fulfillment"}),
        serde_json::json!({"audit_sequence":1,"state":"in_fulfillment","version":null}),
        serde_json::json!({"audit_sequence":1,"state":"invented","version":1}),
        serde_json::json!({"audit_sequence":0,"state":"in_fulfillment","version":1}),
        serde_json::json!({"audit_sequence":9_223_372_036_854_775_808_u64,"state":"in_fulfillment","version":1}),
        serde_json::json!({"audit_sequence":1,"state":"in_fulfillment","version":2_147_483_648_u64}),
        serde_json::json!({"audit_sequence":1.5,"state":"in_fulfillment","version":1}),
        serde_json::json!({"audit_sequence":1,"state":"in_fulfillment","version":1,"extra":false}),
    ];
    for j in bad {
        let mut r = request.clone();
        r.audit_id = Uuid::new_v4();
        r.force_request_observation = Some(j.clone());
        assert!(write(&pg, r).await.is_err(), "{j}");
    }
    let mut missing = request.clone();
    missing.audit_id = u(112);
    missing.force_request_observation = None;
    assert!(write(&pg, missing).await.is_err());
    let mut old_request = request;
    old_request.audit_id = u(113);
    old_request.hash_version = 2;
    old_request.force_request_observation = None;
    write(&pg, old_request).await?;
    for field in [
        "hash_version",
        "entry_hash",
        "outcome",
        "actor_class",
        "trigger",
    ] {
        let mut invalid = audit(Uuid::new_v4().as_u128());
        match field {
            "hash_version" => invalid.hash_version = 9,
            "entry_hash" => invalid.entry_hash = vec![255],
            "outcome" => invalid.outcome = "invented".into(),
            "actor_class" => invalid.actor_class = "admin".into(),
            "trigger" => invalid.trigger = "invented".into(),
            _ => unreachable!(),
        }
        assert!(write(&pg, invalid).await.is_err(), "{field}");
    }
    Ok(())
}
#[tokio::test]
async fn aggregate_foreign_keys_enums_counters_and_closed_compensation() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    // A committed aggregate can never point at a missing version, but same-tx creation works.
    assert!(
        entity::order::Entity::insert(order(1).into_active_model())
            .exec(&pg.raw)
            .await
            .is_err()
    );
    create(&pg.db, 1).await?;
    for update in [
        "state='unknown'",
        "current_version=99,version_allocation_high_water=99",
        "current_version=0",
        "draft_revision=-1",
        "resume_count=-1",
        "amendment_count=-1",
        "fulfillment_control_generation=-1",
        "audit_sequence=-1",
        "state='draft',pre_hold_state='submitted'",
        "sales_path='unknown'",
        "seller_tenant_id='00000000-0000-0000-0000-000000000099'",
    ] {
        assert!(
            pg.sql(&format!("UPDATE bss_orders__order SET {update}"))
                .await
                .is_err(),
            "{update}"
        );
    }
    let mut invalid = version(1, 4, Some(3));
    assert!(
        entity::order_version::Entity::insert(invalid.clone().into_active_model())
            .exec(&pg.raw)
            .await
            .is_err()
    );
    invalid.supersedes_version = Some(1);
    invalid.reason = "unknown".into();
    assert!(
        entity::order_version::Entity::insert(invalid.into_active_model())
            .exec(&pg.raw)
            .await
            .is_err()
    );
    pg.sql("UPDATE bss_orders__order SET version_allocation_high_water=2147483647, draft_revision=9223372036854775807, fulfillment_control_generation=9223372036854775807").await?;
    for update in [
        "version_allocation_high_water=version_allocation_high_water+1",
        "draft_revision=draft_revision+1",
        "fulfillment_control_generation=fulfillment_control_generation+1",
        "version_allocation_high_water=1",
    ] {
        assert!(
            pg.sql(&format!("UPDATE bss_orders__order SET {update}"))
                .await
                .is_err()
        );
    }
    let valid = serde_json::json!({"drafts_voided":[],"activated_rolled_back":[],"activation_dispatched":false,"at_sale_facts_emitted":false,"no_active_subscription_remains":true});
    let mut bad = vec![
        serde_json::json!({}),
        serde_json::json!(null),
        serde_json::json!([]),
    ];
    for key in valid.as_object().unwrap().keys() {
        let mut j = valid.clone();
        j.as_object_mut().unwrap().remove(key);
        bad.push(j);
        let mut j = valid.clone();
        j[key] = serde_json::Value::Null;
        bad.push(j);
    }
    let mut extra = valid.clone();
    extra["extra"] = serde_json::json!(true);
    bad.push(extra);
    for j in bad {
        let result = pg
            .raw
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE bss_orders__order SET compensation_evidence=$1",
                [j.into()],
            ))
            .await;
        assert!(result.is_err());
    }
    pg.raw
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE bss_orders__order SET compensation_evidence=$1",
            [valid.into()],
        ))
        .await?;
    Ok(())
}
#[tokio::test]
async fn nullable_diagnostic_and_policy_keys_keep_business_uniqueness() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let insert = |id: u128, line: &str, item: &str, selected: bool, key: &str| {
        format!(
            "INSERT INTO bss_orders__gate_outcome(outcome_id,run_id,subject_tenant_id,subject_id,resource_tenant_id,seller_tenant_id,payer_tenant_id,line_id,predicate,item_id,has_catalog_selection,catalog_scope_key,verdict,mapping_version,applicability,producer_results,evaluated_at) VALUES('{}','{}','{}','{}','{}','{}','{}',{line},'test',{item},{selected},{key},'passed','v1','required','[]',clock_timestamp())",
            u(id),
            u(100),
            u(10),
            u(40),
            u(10),
            u(20),
            u(30)
        )
    };
    pg.sql(&insert(1, "NULL", "NULL", false, "NULL")).await?;
    assert!(
        pg.sql(&insert(2, "NULL", "NULL", false, "NULL"))
            .await
            .is_err()
    );
    let line = format!("'{}'", u(101));
    let item = format!("'{}'", u(201));
    pg.sql(&insert(3, &line, &item, true, "NULL")).await?;
    pg.sql(&insert(4, &line, &item, true, "'default'")).await?;
    assert!(
        pg.sql(&insert(5, &line, &item, true, "NULL"))
            .await
            .is_err()
    );
    assert!(
        pg.sql(&insert(6, &line, &item, false, "'default'"))
            .await
            .is_err()
    );
    assert!(
        pg.sql(&insert(7, "NULL", &item, true, "NULL"))
            .await
            .is_err()
    );
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__gate_outcome")
            .await?,
        3
    );
    assert!(
        pg.sql(
            "INSERT INTO bss_orders__date_policy VALUES(gen_random_uuid(),NULL,false,false,1,now())"
        )
        .await
        .is_err()
    );
    assert!(pg.sql("INSERT INTO bss_orders__state_ttl_policy VALUES(gen_random_uuid(),'platform',NULL,'draft',interval '1 month',false,1,'promotion',now())").await.is_err());
    assert!(pg.sql("INSERT INTO bss_orders__state_ttl_policy VALUES(gen_random_uuid(),'platform',NULL,'in_fulfillment',interval '1 month',false,1,'promotion',now())").await.is_err());
    pg.sql("INSERT INTO bss_orders__policy_election VALUES(gen_random_uuid(),'acceptance_required','platform',NULL,true,'promotion',now())").await?;
    assert!(pg.sql("INSERT INTO bss_orders__policy_election VALUES(gen_random_uuid(),'acceptance_required','platform',NULL,false,'promotion',now())").await.is_err());
    assert!(pg.sql("INSERT INTO bss_orders__policy_election VALUES(gen_random_uuid(),'acceptance_required','seller',NULL,true,'promotion',now())").await.is_err());
    Ok(())
}

#[tokio::test]
async fn overlap_claim_collision_is_nonfatal_and_only_release_is_mutable() -> anyhow::Result<()> {
    use super::claims::{create_for, lock, replace, replace_in, rows, scope};
    use crate::domain::overlap::ClaimOutcome;
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create_for(&db, 1, 30, 10).await?;
    create_for(&db, 2, 30, 10).await?;
    assert!(matches!(
        replace(&db, 1, 30, &["owner-key"], 9, &[1]).await?,
        ClaimOutcome::Admitted { .. }
    ));
    // A collision is a row shortfall: the transaction stays usable for refusal audit/settlement.
    db.transaction_ref_mapped_with_config(
        crate::infra::storage::repo::claims::transition_tx_config(),
        |tx| {
            Box::pin(async move {
                let out = replace_in(tx, 2, 30, &["owner-key".to_owned()], 9, &[2]).await?;
                assert!(matches!(out, ClaimOutcome::Conflict(_)));
                assert!(repo::find_order(tx, &scope(2), u(2)).await?.is_some());
                anyhow::Ok(())
            })
        },
    )
    .await?;
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim")
            .await?,
        1
    );
    for sql in [
        "UPDATE bss_orders__inflight_overlap_claim SET version=10",
        "UPDATE bss_orders__inflight_overlap_claim SET overlap_scope_key='different'",
        "DELETE FROM bss_orders__inflight_overlap_claim",
    ] {
        assert!(pg.sql(sql).await.is_err());
    }
    let held = rows(&pg, 1).await?[0].claim_id;
    let mismatch: anyhow::Result<()> = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let locked = lock(tx, 1).await?;
                // An ID outside this parent: the count mismatch must abort, and the caller's
                // error rolls back the one row the statement did touch.
                assert!(locked.release_claims(&[held, u(102)]).await.is_err());
                anyhow::bail!("abort after release mismatch")
            })
        })
        .await;
    assert!(mismatch.is_err());
    assert_eq!(
        pg.scalar(
            "SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE released_at IS NULL"
        )
        .await?,
        1
    );
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let locked = lock(tx, 1).await?;
            locked.release_claims(&[held]).await?;
            // A second release of the same ID affects no live row and must fail.
            assert!(locked.release_claims(&[held]).await.is_err());
            anyhow::Ok(())
        })
    })
    .await?;
    assert!(
        pg.sql("UPDATE bss_orders__inflight_overlap_claim SET released_at=NULL")
            .await
            .is_err()
    );
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE released_at IS NOT NULL").await?,1);
    // Release time is the database clock, not a caller-supplied instant.
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE released_at > now() - interval '1 minute'").await?,1);
    Ok(())
}

#[tokio::test]
async fn acceptance_totals_reflections_and_projection_have_real_keys() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    create(&pg.db, 1).await?;
    entity::order_version::Entity::insert(version(1, 4, Some(1)).into_active_model())
        .exec(&pg.raw)
        .await?;
    pg.sql(&format!(
        "INSERT INTO bss_orders__order_line_identity VALUES('{}','{}',now()),('{}','{}',now())",
        u(1),
        u(101),
        u(1),
        u(102)
    ))
    .await?;
    let accept = |v| {
        format!(
            "INSERT INTO bss_orders__acceptance VALUES('{}',{v},now(),'{}','self_service','volunteered')",
            u(1),
            u(40)
        )
    };
    assert!(pg.sql(&accept(1)).await.is_err());
    assert!(pg.sql(&accept(3)).await.is_err());
    pg.sql(&accept(4)).await?;
    assert!(pg.sql(&accept(4)).await.is_err());
    assert!(
        pg.sql(&format!(
            "INSERT INTO bss_orders__acceptance VALUES('{}',4,NULL,'{}','self_service','contract')",
            u(1),
            u(40)
        ))
        .await
        .is_err()
    );
    let reflection = |id: u128, kind: &str, verdict: &str, reason: &str| {
        format!(
            "INSERT INTO bss_orders__approval_reflection VALUES('{}','{}',4,'{kind}','{verdict}','policy',{reason},'{}',now())",
            u(id),
            u(1),
            u(99)
        )
    };
    assert!(
        pg.sql(&reflection(1, "requirement", "granted", "NULL"))
            .await
            .is_err()
    );
    assert!(
        pg.sql(&reflection(1, "gate_outcome", "denied", "NULL"))
            .await
            .is_err()
    );
    pg.sql(&reflection(1, "requirement", "required", "NULL"))
        .await?;
    pg.sql(&reflection(2, "gate_outcome", "denied", "'why'"))
        .await?;
    assert!(
        pg.sql(&reflection(3, "requirement", "not_required", "NULL"))
            .await
            .is_err()
    );
    let total = |scope: &str,
                 line: u128,
                 kind: &str,
                 status: &str,
                 gross: &str,
                 tcv: &str,
                 basis: &str,
                 evidence: &str| {
        format!(
            "INSERT INTO bss_orders__resolved_total VALUES('{}',4,'{scope}','{}','USD','{}',2,'half_even','{{}}','[]','[]','{status}','tcv','{{}}',{gross},{gross},{gross},NULL,'{kind}',{tcv},{basis},{evidence})",
            u(1),
            u(line),
            u(88)
        )
    };
    pg.sql(&total(
        "order",
        0,
        "recurring",
        "committed",
        "10",
        "10",
        "'finite_term'",
        "'{}'",
    ))
    .await?;
    pg.sql(&total(
        "line",
        101,
        "usage",
        "uncommitted_usage",
        "NULL",
        "NULL",
        "NULL",
        "NULL",
    ))
    .await?;
    assert!(
        pg.sql(&total(
            "line",
            999,
            "one_time",
            "committed",
            "1",
            "NULL",
            "NULL",
            "NULL"
        ))
        .await
        .is_err()
    );
    assert!(
        pg.sql(&total(
            "order",
            0,
            "usage",
            "committed",
            "0",
            "NULL",
            "NULL",
            "NULL"
        ))
        .await
        .is_err()
    );
    assert!(
        pg.sql(&total(
            "order",
            101,
            "usage",
            "uncommitted_usage",
            "NULL",
            "NULL",
            "NULL",
            "NULL"
        ))
        .await
        .is_err()
    );
    assert!(pg.sql(&format!("INSERT INTO bss_orders__line_fulfillment VALUES('{}','{}',4,'activated','{}',NULL,now())",u(1),u(101),u(200))).await.is_err()); // no committed line
    pg.sql(&format!("INSERT INTO bss_orders__order_line(order_id,version,line_id,plan_id,plan_revision_id,selected_items,currency,contract_effective_date,service_activation_date,acceptance_due_date,term_kind,authored_term,billing_cycle,date_policy_switch_state) VALUES('{}',4,'{}','{}','{}','[]','USD',CURRENT_DATE,CURRENT_DATE,CURRENT_DATE,'rolling','{{\"kind\":\"rolling\"}}','month','{}')",u(1),u(101),u(200),u(201),PLATFORM_SNAPSHOT)).await?;
    let projection = format!(
        "INSERT INTO bss_orders__line_fulfillment VALUES('{}','{}',4,'activated','{}',NULL,now())",
        u(1),
        u(101),
        u(200)
    );
    pg.sql(&projection).await?;
    assert!(pg.sql(&projection).await.is_err());
    Ok(())
}

#[tokio::test]
async fn concurrent_overlap_claims_have_exactly_one_winner() -> anyhow::Result<()> {
    use super::claims::{contend, create_for, live};
    use crate::domain::overlap::ClaimOutcome;
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    let keys = |round: u128, ks: &[&str]| -> Vec<String> {
        ks.iter().map(|k| format!("r{round}-{k}")).collect()
    };
    // Repeated rounds: single key, multi-key baskets in reversed caller order, and baskets
    // overlapping on one tuple. Every round must finish without any error (a deadlock abort
    // would surface as one), with exactly one winner and no live claim left to the loser.
    for round in 0..12u128 {
        let (a, b) = (1000 + 2 * round, 1001 + 2 * round);
        create_for(&db, a, 30, 10).await?;
        create_for(&db, b, 30, 10).await?;
        let (ka, kb) = match round % 3 {
            0 => (keys(round, &["k"]), keys(round, &["k"])),
            1 => (
                keys(round, &["k1", "k2", "k3"]),
                keys(round, &["k3", "k2", "k1"]),
            ),
            _ => (keys(round, &["a", "b"]), keys(round, &["c", "b"])),
        };
        let (x, y) = contend(&db, (a, ka.clone()), (b, kb.clone())).await?;
        let won = |o: &ClaimOutcome| matches!(o, ClaimOutcome::Admitted { .. });
        assert!(won(&x) ^ won(&y), "round {round}: {x:?} {y:?}");
        let (winner, loser, winner_keys) = if won(&x) { (a, b, ka) } else { (b, a, kb) };
        let mut expected: Vec<_> = winner_keys.into_iter().map(|k| (u(30), k)).collect();
        expected.sort();
        expected.dedup();
        assert_eq!(live(&pg, winner).await?, expected, "round {round}");
        assert!(live(&pg, loser).await?.is_empty(), "round {round}");
    }
    Ok(())
}
