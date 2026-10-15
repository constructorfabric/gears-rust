use super::*;
use sea_orm::{EntityTrait, IntoActiveModel};
use toolkit_security::{
    AccessScope,
    access_scope::{ScopeConstraint, ScopeFilter},
};
pub(super) fn registry(execution: u128) -> entity::idempotency::Model {
    entity::idempotency::Model {
        operation: "submit".into(),
        principal_scope: "actor".into(),
        idempotency_key: "key".into(),
        order_id: Some(u(1)),
        request_fingerprint: "fingerprint".into(),
        status: "in_flight".into(),
        execution_id: u(execution),
        attempt_id: Some(u(101)),
        fulfillment_control_id: None,
        owner_token: Some(u(300)),
        fencing_generation: 0,
        lease_expires_at: Some(now()),
        outcome: None,
        outcome_reason: None,
        audit_id: None,
        settled_response: None,
        created_at: now() - time::Duration::days(2),
        expires_at: now() - time::Duration::days(1),
    }
}
pub(super) fn attempt() -> entity::commercial_attempt::Model {
    entity::commercial_attempt::Model {
        attempt_id: u(101),
        order_id: u(1),
        candidate_version: 2,
        previous_committed_version: 1,
        idempotency_execution_id: u(201),
        operation: "submit".into(),
        principal_scope: "actor".into(),
        request_fingerprint: "fingerprint".into(),
        prepared_draft_revision: Some(0),
        proposed_arrangement: serde_json::json!({}),
        authorization_fact_fingerprint: "facts".into(),
        original_principal: serde_json::json!({"subject_id":u(40)}),
        proof_reference: None,
        line_requests: serde_json::json!({}),
        date_policy_basis: serde_json::json!({}),
        commercial_subject_id: u(50),
        commercial_subject_type: "service".into(),
        commercial_subject_tenant_id: u(20),
        status: "prepared".into(),
        owner_token: u(300),
        fencing_generation: 0,
        lease_until: Some(now()),
        receipt_results: serde_json::json!({}),
        created_at: now(),
        terminal_at: None,
    }
}
fn scope() -> AccessScope {
    AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
        "principal_scope",
        "actor",
    )]))
}
#[tokio::test]
async fn operational_inputs_first_results_and_fences_cannot_be_rewritten() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    db.transaction_ref_mapped(|tx| {
        Box::pin(async move {
            let target = AccessScope::for_resources(vec![u(1)]);
            let row = repo::find_order(tx, &target, u(1)).await?.unwrap();
            let mut lock = repo::LockedOrder::acquire(tx, &target, &row).await?;
            let mut proposed = row;
            proposed.version_allocation_high_water = 2;
            lock.replace(&target, proposed).await?;
            lock.insert_commercial_attempt(attempt()).await?;
            assert!(repo::private::offer_idempotency(tx, &scope(), registry(201)).await?);
            anyhow::Ok(())
        })
    })
    .await?;
    assert!(
        repo::private::find_idempotency(
            &db.conn()?,
            &AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
                "principal_scope",
                "other"
            )])),
            "submit",
            "actor",
            "key"
        )
        .await?
        .is_none()
    );
    for sql in [
        "UPDATE bss_orders__commercial_attempt SET request_fingerprint='changed'",
        "UPDATE bss_orders__commercial_attempt SET commercial_subject_id=gen_random_uuid()",
        "UPDATE bss_orders__commercial_attempt SET owner_token=gen_random_uuid()",
        "UPDATE bss_orders__commercial_attempt SET fencing_generation=-1",
        "DELETE FROM bss_orders__commercial_attempt",
        "DELETE FROM bss_orders__idempotency",
    ] {
        assert!(pg.sql(sql).await.is_err(), "{sql}");
    }
    db.transaction_ref_mapped(|tx| {
        Box::pin(async move {
            let target = AccessScope::for_resources(vec![u(1)]);
            let row = repo::find_order(tx, &target, u(1)).await?.unwrap();
            let lock = repo::LockedOrder::acquire(tx, &target, &row).await?;
            let mut result = attempt();
            result.receipt_results = serde_json::json!({"line":{"receipt":"first"}});
            lock.replace_commercial_attempt(&attempt(), result.clone())
                .await?;
            // A stale expected row is rejected before any UPDATE.
            assert!(
                lock.replace_commercial_attempt(&attempt(), result)
                    .await
                    .is_err()
            );
            anyhow::Ok(())
        })
    })
    .await?;
    for value in [
        "{}",
        "{\"line\":{\"receipt\":\"second\"}}",
        "{\"line\":{\"receipt\":\"first\",\"extra\":true}}",
    ] {
        assert!(
            pg.sql(&format!(
                "UPDATE bss_orders__commercial_attempt SET receipt_results='{value}'"
            ))
            .await
            .is_err()
        );
    }
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order_version")
            .await?,
        1
    );
    // An unrelated marker with the same raw key does not adopt the old immutable execution.
    db.transaction_ref_mapped(|tx| {
        Box::pin(async move {
            let mut competing = registry(202);
            competing.attempt_id = None;
            assert!(!repo::private::offer_idempotency(tx, &scope(), competing).await?);
            assert_eq!(
                repo::private::lock_idempotency(tx, &scope(), "submit", "actor", "key")
                    .await?
                    .unwrap()
                    .execution_id,
                u(201)
            );
            anyhow::Ok(())
        })
    })
    .await?;
    Ok(())
}
#[tokio::test]
async fn only_one_live_control_and_exact_receiver_evidence_survive_recovery() -> anyhow::Result<()>
{
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create(&db, 1).await?;
    let control = entity::fulfillment_control::Model {
        control_id: u(102),
        order_id: u(1),
        idempotency_execution_id: u(202),
        operation: "hold".into(),
        request_fingerprint: "fingerprint".into(),
        expected_version: 1,
        fulfillment_attempt_id: u(99),
        generation: 1,
        original_actor: serde_json::json!({"subject_id":u(40)}),
        proof_reference: None,
        authorization_fact_fingerprint: "facts".into(),
        roster: serde_json::json!([]),
        roster_digest: "digest".into(),
        created_at: now(),
        status: "prepared".into(),
        owner_token: u(300),
        fencing_generation: 0,
        lease_until: Some(now()),
        receiver_commands: serde_json::json!({"r":{"command":"first"}}),
        receiver_evidence: serde_json::json!({"r":{"status":"paused"}}),
        error_classification: None,
        terminal_at: None,
    };
    let initial = control.clone();
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let target = AccessScope::for_resources(vec![u(1)]);
            let row = repo::find_order(tx, &target, u(1)).await?.unwrap();
            let lock = repo::LockedOrder::acquire(tx, &target, &row).await?;
            lock.insert_fulfillment_control(initial).await?;
            let mut marker = registry(202);
            marker.operation = "hold".into();
            marker.attempt_id = None;
            marker.fulfillment_control_id = Some(u(102));
            repo::private::insert_idempotency(tx, &scope(), marker).await?;
            anyhow::Ok(())
        })
    })
    .await?;
    for sql in [
        "UPDATE bss_orders__fulfillment_control SET generation=2",
        "UPDATE bss_orders__fulfillment_control SET roster='[{}]'",
        "UPDATE bss_orders__fulfillment_control SET receiver_commands='{\"r\":{\"command\":\"first\",\"extra\":true}}'",
        "UPDATE bss_orders__fulfillment_control SET receiver_evidence='{}'",
        "DELETE FROM bss_orders__fulfillment_control",
        "DELETE FROM bss_orders__idempotency",
    ] {
        assert!(pg.sql(sql).await.is_err(), "{sql}");
    }

    let mut duplicate = control;
    duplicate.control_id = u(103);
    duplicate.idempotency_execution_id = u(203);
    assert!(
        entity::fulfillment_control::Entity::insert(duplicate.into_active_model())
            .exec(&pg.raw)
            .await
            .is_err()
    );
    Ok(())
}
