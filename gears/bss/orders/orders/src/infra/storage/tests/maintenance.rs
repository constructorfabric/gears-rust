//! D-184 bounded maintenance authority on real PostgreSQL: read-only discovery, narrowed
//! targets, allowlisted tasks and independence from PDP outages.
use super::*;
use crate::authz::test_pdp::{ScriptedPdp, pep, user};
use crate::authz::{Action, AuthzFailure, Prefetch};
use crate::infra::maintenance::scope::{
    DiscoveryError, discover_expired_idempotency, discover_orders, discover_retention, read_only,
};
use crate::infra::maintenance::{
    MaintenanceAuthority, MaintenanceTask, RetentionTable, ServiceActor, TargetScope,
    lock_target_order,
};
use sea_orm::{EntityTrait, IntoActiveModel};
use toolkit_db::secure::SecureInsertExt;

fn authority(tasks: &[MaintenanceTask]) -> MaintenanceAuthority {
    MaintenanceAuthority::configured(
        ServiceActor::configured(u(900), u(901)).unwrap(),
        tasks.iter().copied(),
    )
}

#[test]
fn maintenance_identity_and_tasks_are_explicit() {
    assert!(ServiceActor::configured(Uuid::nil(), u(1)).is_none());
    assert!(ServiceActor::configured(u(1), Uuid::nil()).is_none());
    let authority = authority(&[MaintenanceTask::RetentionPurge]);
    assert!(authority.grant(MaintenanceTask::RetentionPurge).is_some());
    assert!(authority.grant(MaintenanceTask::StateExpiry).is_none());
    let grant = authority.grant(MaintenanceTask::RetentionPurge).unwrap();
    assert_eq!(grant.actor().subject_id(), u(900));
    assert_eq!(grant.task(), MaintenanceTask::RetentionPurge);
    // Configuration: absent authority fails every task closed; nil actors fail validation.
    let config: crate::config::OrdersConfig = serde_json::from_value(serde_json::json!({
        "lock_route": "direct", "idempotency_lease_seconds": 30, "dependency_timeout_ms": 1000,
        "service_principals": [{"role": "workflow", "subject_id": u(77), "tenant_id": u(1)}],
        "events": {"producer_subject_id": u(78), "producer_tenant_id": u(1), "broker_partitions": 4},
        "audit_minimization": {"key_id": "t1", "key": "0123456789abcdef0123456789abcdef"}
    }))
    .unwrap();
    assert!(config.maintenance_authority().is_none());
    let nil: crate::config::OrdersConfig = serde_json::from_value(serde_json::json!({
        "lock_route": "direct", "idempotency_lease_seconds": 30, "dependency_timeout_ms": 1000,
        "service_principals": [{"role": "workflow", "subject_id": u(77), "tenant_id": u(1)}],
        "events": {"producer_subject_id": u(78), "producer_tenant_id": u(1), "broker_partitions": 4},
        "audit_minimization": {"key_id": "t1", "key": "0123456789abcdef0123456789abcdef"},
        "maintenance": {"actor_subject_id": Uuid::nil(), "actor_tenant_id": u(1), "tasks": ["retention_purge"]}
    }))
    .unwrap();
    assert!(nil.validate().is_err());
    let unknown = serde_json::from_value::<crate::config::OrdersConfig>(serde_json::json!({
        "lock_route": "direct", "idempotency_lease_seconds": 30, "dependency_timeout_ms": 1000,
        "service_principals": [{"role": "workflow", "subject_id": u(77), "tenant_id": u(1)}],
        "events": {"producer_subject_id": u(78), "producer_tenant_id": u(1), "broker_partitions": 4},
        "audit_minimization": {"key_id": "t1", "key": "0123456789abcdef0123456789abcdef"},
        "maintenance": {"actor_subject_id": u(1), "actor_tenant_id": u(1), "tasks": ["purge_everything"]}
    }));
    assert!(unknown.is_err());
}

/// A write inside the exact discovery transaction fails on PostgreSQL as read-only (D-184).
#[tokio::test]
async fn discovery_transaction_rejects_writes_on_postgres() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let result = read_only(&pg.db, |tx, _discovery| {
        Box::pin(async move {
            let model = order(5).into_active_model();
            entity::order::Entity::insert(model.clone())
                .secure()
                .scope_with_model(&toolkit_security::AccessScope::for_resource(u(5)), &model)?
                .exec(tx)
                .await?;
            Ok(())
        })
    })
    .await;
    let message = format!("{:?}", result.unwrap_err());
    assert!(message.contains("read-only transaction"), "{message}");
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order")
            .await?,
        0
    );
    Ok(())
}

/// Discovery is task-gated and bounded; targets carry only persisted facts and are rechecked.
#[tokio::test]
async fn discovered_targets_are_narrow_and_rechecked_under_lock() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (runtime, _) = pg.role("runtime").await?;
    create(&runtime, 1).await?;
    create(&runtime, 2).await?;
    let (discovery, _) = pg.role("discovery").await?;
    let expiry = authority(&[MaintenanceTask::DraftAutoVoid]);
    let grant = expiry.grant(MaintenanceTask::DraftAutoVoid).unwrap();
    let later = now() + time::Duration::days(1);
    assert!(matches!(
        discover_orders(&discovery, &grant, "draft", later, 0).await,
        Err(DiscoveryError::Batch)
    ));
    assert!(matches!(
        discover_orders(&discovery, &grant, "draft", later, 5001).await,
        Err(DiscoveryError::Batch)
    ));
    let cleanup = authority(&[MaintenanceTask::IdempotencyCleanup]);
    let wrong = cleanup.grant(MaintenanceTask::IdempotencyCleanup).unwrap();
    assert!(matches!(
        discover_orders(&discovery, &wrong, "draft", later, 10).await,
        Err(DiscoveryError::NotGranted)
    ));
    let found = discover_orders(&discovery, &grant, "draft", later, 1).await?;
    assert_eq!(found.len(), 1, "bounded batch");
    let found = discover_orders(&discovery, &grant, "draft", later, 10).await?;
    assert_eq!(found.len(), 2);
    let target = TargetScope::from_discovered_order(&found[0]);
    let id = found[0].order_id();
    // The target narrows to exactly the discovered order and its stored axes.
    let locked_id = runtime
        .transaction_ref_mapped({
            let target = target.clone();
            move |tx| {
                Box::pin(async move {
                    Ok(lock_target_order(tx, &target)
                        .await?
                        .map(|locked| locked.row().order_id))
                })
            }
        })
        .await
        .map_err(|e: anyhow::Error| e)?;
    assert_eq!(locked_id, Some(id));
    // A changed persisted fact (state) makes the candidate ineligible, never re-targeted.
    pg.sql(&format!(
        "UPDATE bss_orders__order SET state_entered_at = state_entered_at + interval '1 second' WHERE order_id='{id}'"
    ))
    .await?;
    let relocked = runtime
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move { Ok(lock_target_order(tx, &target).await?.is_some()) })
        })
        .await
        .map_err(|e: anyhow::Error| e)?;
    assert!(!relocked);
    Ok(())
}

/// Workers keep only their explicit independent capability during a business PDP outage.
#[tokio::test]
async fn workers_purge_and_clean_during_pdp_outage_with_targets_only() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let down = pep(ScriptedPdp::new(|_| None));
    assert!(matches!(
        down.authorize_target(&user(1, 10), Action::OrderRead, &Prefetch::Missing(u(1)))
            .await,
        Err(AuthzFailure::Unavailable)
    ));
    // Two expired read-log rows and one live row.
    for (id, days) in [(1, 100), (2, 95), (3, 1)] {
        pg.sql(&format!(
            "INSERT INTO bss_orders__read_access_log VALUES('{}',NULL,'{}','{}','user','get','refused','order-not-found',NULL,NULL,now()-interval '{days} days')",
            u(id), u(77), u(40)
        ))
        .await?;
    }
    let (retention, _) = pg.role("retention").await?;
    let purge = authority(&[MaintenanceTask::RetentionPurge]);
    let grant = purge.grant(MaintenanceTask::RetentionPurge).unwrap();
    let cutoff = time::OffsetDateTime::now_utc() - time::Duration::days(90);
    let batch = discover_retention(
        &retention,
        &grant,
        RetentionTable::ReadAccessLog,
        cutoff,
        10,
    )
    .await?;
    let target = TargetScope::from_discovered_retention(&batch);
    // The wrong-table entry rejects this target.
    let wrong = target.clone();
    let mismatch: anyhow::Result<u64> = retention
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move { Ok(repo::retention::refused_audit(tx, &wrong).await?) })
        })
        .await;
    assert!(mismatch.is_err());
    let deleted = retention
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move { Ok(repo::retention::access_log(tx, &target).await?) })
        })
        .await
        .map_err(|e: anyhow::Error| e)?;
    assert_eq!(deleted, 2);
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__read_access_log")
            .await?,
        1
    );

    // Registry cleanup: an expired settled marker is deleted through its discovered target.
    pg.sql(&format!(
        "INSERT INTO bss_orders__transition_audit(audit_id,hash_version,subject_tenant_id,requested_order_ref,entry_hash,trigger,outcome,actor,actor_class,reason,idempotency_key,created_at) VALUES('{}',3,'{}','{}',decode(repeat('01',32),'hex'),'cancel','refused','{}','user','order-not-found','old',now()-interval '2 days')",
        u(101), u(10), u(77), u(40)
    ))
    .await?;
    pg.sql(&format!("INSERT INTO bss_orders__idempotency(operation,principal_scope,idempotency_key,request_fingerprint,status,execution_id,fencing_generation,outcome,outcome_reason,audit_id,settled_response,created_at,expires_at) VALUES('create','actor','old','hash','settled','{}',0,'refused','order-not-found','{}','{{\"formatVersion\":1,\"body\":{{\"reason\":\"order-not-found\"}}}}',now()-interval '2 days',now()-interval '1 day')", u(201), u(101))).await?;
    let (maintenance, _) = pg.role("maintenance").await?;
    let cleanup = authority(&[MaintenanceTask::IdempotencyCleanup]);
    let grant = cleanup.grant(MaintenanceTask::IdempotencyCleanup).unwrap();
    let expired =
        discover_expired_idempotency(&maintenance, &grant, time::OffsetDateTime::now_utc(), 10)
            .await?;
    assert_eq!(expired.len(), 1);
    let target = TargetScope::from_discovered_idempotency(&expired[0]);
    let removed = maintenance
        .transaction_ref_mapped(move |tx| {
            Box::pin(
                async move { Ok(repo::private::delete_expired_idempotency(tx, &target).await?) },
            )
        })
        .await
        .map_err(|e: anyhow::Error| e)?;
    assert!(removed);
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__idempotency")
            .await?,
        0
    );
    Ok(())
}

/// Gap review (S2-03): namespace discovery returns distinct namespaces under the batch bound and
/// pages by keyset, so a namespace with many committed rows never hides the others.
#[tokio::test]
async fn audit_namespace_discovery_is_distinct_and_pages_past_busy_namespaces() -> anyhow::Result<()>
{
    use crate::infra::maintenance::scope::discover_audit_namespaces;
    let pg = Pg::new().await?;
    for (id, namespace) in [(1u128, 10u128), (2, 10), (3, 10), (4, 11)] {
        let mut row = order(id);
        row.resource_tenant_id = u(namespace);
        row.audit_tenant_id = u(namespace);
        let scope = toolkit_security::AccessScope::for_resources(vec![u(id)]);
        pg.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let row = repo::insert_order(tx, &scope, row).await?;
                    let locked = repo::LockedOrder::acquire(tx, &scope, &row).await?;
                    locked.insert_order_version(version(id, 1, None)).await?;
                    anyhow::Ok(())
                })
            })
            .await?;
        let mut evidence = super::constraints::audit(100 + id);
        evidence.trigger = "create".into();
        evidence.reason = "create".into();
        evidence.outcome = "committed".into();
        evidence.order_id = Some(u(id));
        evidence.requested_order_ref = Some(u(id));
        evidence.audit_tenant_id = Some(u(namespace));
        evidence.resource_tenant_id = Some(u(namespace));
        evidence.sequence = Some(1);
        evidence.prev_hash = Some(vec![0; 32]);
        evidence.entry_hash = vec![u8::try_from(id)?; 32];
        evidence.to_state = Some("draft".into());
        evidence.version = Some(1);
        super::constraints::write(&pg, evidence).await?;
    }
    let authority = authority(&[MaintenanceTask::AuditVerification]);
    let grant = authority.grant(MaintenanceTask::AuditVerification).unwrap();
    let ids = |found: Vec<crate::infra::maintenance::scope::DiscoveredAuditNamespace>| {
        found
            .iter()
            .map(|n| TargetScope::from_discovered_audit_namespace(n).audit_namespace())
            .collect::<Vec<_>>()
    };
    // Three committed rows of namespace 10 must not consume a batch of two.
    assert_eq!(
        ids(discover_audit_namespaces(&pg.db, &grant, None, 2).await?),
        vec![Some(u(10)), Some(u(11))]
    );
    assert_eq!(
        ids(discover_audit_namespaces(&pg.db, &grant, None, 1).await?),
        vec![Some(u(10))]
    );
    assert_eq!(
        ids(discover_audit_namespaces(&pg.db, &grant, Some(u(10)), 1).await?),
        vec![Some(u(11))]
    );
    assert!(
        discover_audit_namespaces(&pg.db, &grant, Some(u(11)), 1)
            .await?
            .is_empty()
    );
    Ok(())
}
