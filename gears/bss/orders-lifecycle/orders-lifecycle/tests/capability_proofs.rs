//! S1-03: real PostgreSQL + real PEP/compiler/secure ORM, with recorded policy decisions.
mod capability_support;
use authz_resolver_sdk::{
    EnforcerError, EvaluationResponse, EvaluationResponseContext, PolicyEnforcer,
};
use capability_support::{
    Pg, capabilities, entity, exact_scope, flows, history_insert, insert, provider, row, rows,
};
use sea_orm::{EntityTrait, IntoActiveModel, NotSet, Set};
use std::sync::Arc;
use toolkit_db::secure::{
    SecureEntityExt, SecureInsertExt, secure_insert, secure_update_with_scope,
};
use toolkit_db::{LockConfig, secure::TxConfig};
use toolkit_security::AccessScope;
use uuid::Uuid;
fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

#[tokio::test]
async fn three_axes_and_ids_keep_complete_or_paths_in_sql_and_inserts() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let a = row(1, 10, 20, 30);
    let b = row(2, 11, 21, 31);
    let mixed = row(3, 10, 21, 30);
    let same_axes = row(4, 10, 20, 30);
    for value in [&a, &b, &mixed, &same_axes] {
        insert(&pg.db, value).await?;
    }
    let branches = vec![
        provider::path(vec![
            provider::eq("id", a.order_id),
            provider::eq("resource_tenant_id", u(10)),
            provider::eq("seller_tenant_id", u(20)),
            provider::eq("payer_tenant_id", u(30)),
        ]),
        provider::path(vec![
            provider::eq("id", b.order_id),
            provider::eq("resource_tenant_id", u(11)),
            provider::eq("seller_tenant_id", u(21)),
            provider::eq("payer_tenant_id", u(31)),
        ]),
    ];
    let scope = provider::scope(branches).await;
    let found = rows(&pg.db, &scope).await?;
    assert_eq!(found.len(), 2);
    assert!(found.contains(&a) && found.contains(&b));
    assert!(flows::validate_proposed(&mixed, &scope).is_err());
    assert!(flows::validate_proposed(&same_axes, &scope).is_err());
    for axis in ["resource_tenant_id", "seller_tenant_id", "payer_tenant_id"] {
        let wrong = provider::scope(vec![provider::path(vec![
            provider::eq("id", a.order_id),
            provider::eq(axis, u(999)),
        ])])
        .await;
        assert!(rows(&pg.db, &wrong).await?.is_empty());
        assert!(flows::validate_proposed(&a, &wrong).is_err());
    }
    // Drop ID to ensure mixed branches cannot be spliced even when each value occurs somewhere.
    let scope = provider::scope(vec![
        provider::path(vec![
            provider::eq("resource_tenant_id", u(10)),
            provider::eq("seller_tenant_id", u(20)),
        ]),
        provider::path(vec![
            provider::eq("resource_tenant_id", u(11)),
            provider::eq("seller_tenant_id", u(21)),
        ]),
    ])
    .await;
    assert!(!rows(&pg.db, &scope).await?.contains(&mixed));
    assert!(flows::validate_proposed(&mixed, &scope).is_err());
    let denied = row(5, 99, 99, 99);
    assert!(
        secure_insert::<entity::Entity>(denied.into_active_model(), &scope, &pg.db.conn()?)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn proposed_values_and_locked_facts_are_separate_requirements() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let original = row(1, 10, 20, 30);
    insert(&pg.db, &original).await?;
    let old_scope = exact_scope(&original).await;
    let mut proposed = original.clone();
    proposed.payer_tenant_id = u(31);
    proposed.resource_tenant_id = u(11);
    proposed.version = 2;
    let new_scope = exact_scope(&proposed).await;
    assert!(
        flows::guarded_update(
            &pg.db,
            original.clone(),
            proposed.clone(),
            old_scope.clone(),
            old_scope.clone()
        )
        .await
        .is_err()
    );
    assert!(
        flows::guarded_update(
            &pg.db,
            original.clone(),
            proposed.clone(),
            new_scope.clone(),
            new_scope.clone()
        )
        .await
        .is_err()
    );
    assert_eq!(rows(&pg.db, &old_scope).await?, vec![original.clone()]);
    let mut seller_changed = proposed.clone();
    seller_changed.seller_tenant_id = u(21);
    assert!(
        flows::guarded_update(
            &pg.db,
            original.clone(),
            seller_changed.clone(),
            old_scope.clone(),
            exact_scope(&seller_changed).await
        )
        .await
        .is_err()
    );
    flows::guarded_update(
        &pg.db,
        original.clone(),
        proposed.clone(),
        old_scope,
        new_scope.clone(),
    )
    .await?;
    assert_eq!(rows(&pg.db, &new_scope).await?, vec![proposed.clone()]);
    // A policy scope broad enough to see the new row cannot bypass the snapshot recheck.
    let broad = provider::scope(vec![provider::path(vec![provider::eq(
        "id",
        original.order_id,
    )])])
    .await;
    assert!(
        flows::guarded_update(
            &pg.db,
            original.clone(),
            proposed.clone(),
            broad.clone(),
            new_scope
        )
        .await
        .is_err()
    );
    // The raw helper alone does NOT authorize proposed custom axes. This pins the adapter requirement.
    let mut active = proposed.clone().into_active_model();
    active.payer_tenant_id = Set(u(999));
    secure_update_with_scope::<entity::Entity>(active, &broad, proposed.order_id, &pg.db.conn()?)
        .await?;
    assert_eq!(rows(&pg.db, &broad).await?[0].payer_tenant_id, u(999));
    // A partial active model silently skips NotSet scope fields; complete-model validation refuses.
    let mut partial = original.clone().into_active_model();
    partial.payer_tenant_id = NotSet;
    let wrong = provider::scope(vec![provider::path(vec![provider::eq(
        "payer_tenant_id",
        u(999),
    )])])
    .await;
    assert!(
        entity::Entity::insert(partial.clone())
            .secure()
            .scope_with_model(&wrong, &partial)
            .is_ok()
    );
    assert!(flows::validate_proposed(&original, &wrong).is_err());
    Ok(())
}

#[tokio::test]
async fn hidden_missing_and_historical_reads_obey_current_parent() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let original = row(1, 10, 20, 30);
    insert(&pg.db, &original).await?;
    history_insert(&pg.db, &original).await?;
    let denied = provider::Recorded::new(Some(EvaluationResponse {
        decision: false,
        context: EvaluationResponseContext {
            constraints: vec![],
            deny_reason: None,
        },
    }));
    for id in [u(1), u(999)] {
        assert_eq!(
            flows::point_read(&pg.db, id, &denied).await?,
            flows::PointResult::HiddenOrMissing
        );
    }
    assert_eq!(denied.requests.lock().len(), 2);
    {
        let requests = denied.requests.lock();
        assert_eq!(requests[0].resource.id, Some(u(1)));
        for (name, value) in [
            ("id", u(1)),
            ("resource_tenant_id", u(10)),
            ("seller_tenant_id", u(20)),
            ("payer_tenant_id", u(30)),
        ] {
            assert_eq!(
                requests[0].resource.properties.get(name),
                Some(&serde_json::json!(value))
            );
        }
    }
    let allowed = provider::Recorded::new(Some(provider::allow(vec![provider::path(vec![
        provider::eq("id", u(1)),
    ])])));
    assert_eq!(
        flows::point_read(&pg.db, u(1), &allowed).await?,
        flows::PointResult::Found(original.clone())
    );
    assert_eq!(
        flows::point_read(&pg.db, u(999), &allowed).await?,
        flows::PointResult::HiddenOrMissing
    );
    let unavailable = provider::Recorded::new(None);
    for id in [u(1), u(999)] {
        assert_eq!(
            flows::point_read(&pg.db, id, &unavailable).await?,
            flows::PointResult::Unavailable
        );
    }
    assert_eq!(unavailable.requests.lock().len(), 2);
    let old_scope = exact_scope(&original).await;
    assert_eq!(flows::historical(&pg.db, u(1), &old_scope).await?.len(), 1);
    let mut new = original.clone();
    new.payer_tenant_id = u(31);
    new.version = 2;
    let new_scope = exact_scope(&new).await;
    flows::guarded_update(&pg.db, original, new, old_scope.clone(), new_scope.clone()).await?;
    assert!(
        flows::historical(&pg.db, u(1), &old_scope)
            .await?
            .is_empty()
    );
    assert_eq!(
        flows::historical(&pg.db, u(1), &new_scope).await?[0].old_payer_id,
        u(30)
    );
    Ok(())
}

#[tokio::test]
async fn discovery_targets_read_only_transactions_and_direct_session_locks() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let original = row(1, 10, 20, 30);
    insert(&pg.db, &original).await?;
    let discovery_db = pg.role("probe_discovery").await?;
    let targets = capabilities::discover(&discovery_db).await?;
    let target = capabilities::TargetScope::from_discovered(
        targets.into_iter().next().expect("persisted row"),
    );
    assert!(capabilities::recheck_target(&pg.db, target).await?);
    let scope = exact_scope(&original).await;
    let mut second = original.clone();
    second.order_id = u(2);
    let second_scope = exact_scope(&second).await;
    let failed: anyhow::Result<()> = pg
        .db
        .transaction_ref_mapped_with_config(TxConfig::read_only(), |tx| {
            Box::pin(async move {
                secure_insert::<entity::Entity>(second.into_active_model(), &second_scope, tx)
                    .await?;
                Ok(())
            })
        })
        .await;
    assert!(
        format!(
            "{:#}",
            failed.expect_err("read-only transaction must reject writes")
        )
        .contains("read-only")
    );
    assert_eq!(rows(&pg.db, &scope).await?.len(), 1);
    // Two independent pools, real PostgreSQL session advisory lock, no time-based test sleeps.
    let peer = pg.role("probe_maintenance").await?;
    let guard = pg.db.lock("orders-capability", "one-order").await?;
    let config = LockConfig {
        max_retries: Some(0),
        ..LockConfig::default()
    };
    assert!(
        peer.try_lock("orders-capability", "one-order", config.clone())
            .await?
            .is_none()
    );
    guard.release().await?;
    let next = peer
        .try_lock("orders-capability", "one-order", config)
        .await?
        .expect("released lock");
    next.release().await?;
    Ok(())
}

#[tokio::test]
async fn provider_and_finite_service_scope_contracts_fail_closed() {
    let real = PolicyEnforcer::new(Arc::new(provider::BundledStatic));
    let result = real
        .access_scope_with(
            &provider::caller(),
            &bss_orders_lifecycle::gts::ORDER,
            "read",
            Some(u(1)),
            &flows::properties(&row(1, 10, 20, 30)),
        )
        .await;
    assert!(matches!(result, Err(EnforcerError::CompileFailed(_))));
    let empty = provider::Recorded::new(Some(provider::allow(vec![])));
    assert!(matches!(
        empty
            .enforcer()
            .access_scope(
                &provider::caller(),
                &bss_orders_lifecycle::gts::ORDER,
                "read",
                None
            )
            .await,
        Err(EnforcerError::CompileFailed(_))
    ));
    let bounded = provider::scope(vec![provider::path(vec![
        provider::eq("id", u(1)),
        provider::eq("seller_tenant_id", u(20)),
    ])])
    .await;
    assert!(flows::finite_ids(&bounded));
    let one_unbounded_branch = provider::scope(vec![
        provider::path(vec![provider::eq("id", u(1))]),
        provider::path(vec![provider::eq("seller_tenant_id", u(20))]),
    ])
    .await;
    assert!(!flows::finite_ids(&one_unbounded_branch));
    assert!(!flows::finite_ids(&AccessScope::deny_all()));
}

#[tokio::test]
async fn database_roles_enforce_private_evidence_boundaries() -> anyhow::Result<()> {
    use capability_support::evidence::{audit, checkpoint, private};
    use toolkit_db::secure::SecureDeleteExt;
    let pg = Pg::new().await?;
    let scope = AccessScope::for_resources(vec![u(1)]);
    let a = audit::ActiveModel {
        id: Set(u(1)),
        value: Set("evidence".into()),
    };
    let p = private::ActiveModel {
        id: Set(u(1)),
        value: Set("private".into()),
    };
    let c = checkpoint::ActiveModel {
        id: Set(u(1)),
        value: Set("checkpoint".into()),
    };
    let writer = pg.role("probe_private").await?;
    secure_insert::<audit::Entity>(a.clone(), &scope, &writer.conn()?).await?;
    secure_insert::<private::Entity>(p.clone(), &scope, &writer.conn()?).await?;
    let checkpoint_writer = pg.role("probe_checkpoint").await?;
    secure_insert::<checkpoint::Entity>(c.clone(), &scope, &checkpoint_writer.conn()?).await?;
    let verifier = pg.role("probe_verifier").await?;
    assert_eq!(
        audit::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&verifier.conn()?)
            .await?
            .len(),
        1
    );
    assert_eq!(
        checkpoint::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&verifier.conn()?)
            .await?
            .len(),
        1
    );
    assert!(
        private::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&verifier.conn()?)
            .await
            .is_err()
    );
    for role in [
        "probe_business",
        "probe_private",
        "probe_verifier",
        "probe_checkpoint",
        "probe_retention",
        "probe_maintenance",
        "probe_discovery",
    ] {
        let db = pg.role(role).await?;
        assert!(
            secure_update_with_scope::<audit::Entity>(a.clone(), &scope, u(1), &db.conn()?)
                .await
                .is_err(),
            "{role} must not change audit"
        );
        assert!(
            audit::Entity::delete_many()
                .secure()
                .scope_with(&scope)
                .exec(&db.conn()?)
                .await
                .is_err(),
            "{role} must not delete audit"
        );
        assert!(
            secure_update_with_scope::<checkpoint::Entity>(c.clone(), &scope, u(1), &db.conn()?)
                .await
                .is_err(),
            "{role} must not change checkpoint"
        );
        assert!(
            checkpoint::Entity::delete_many()
                .secure()
                .scope_with(&scope)
                .exec(&db.conn()?)
                .await
                .is_err(),
            "{role} must not delete checkpoint"
        );
    }
    let original = row(1, 10, 20, 30);
    let business = pg.role("probe_business").await?;
    insert(&business, &original).await?;
    assert!(
        audit::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&business.conn()?)
            .await
            .is_err()
    );
    assert!(
        private::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&business.conn()?)
            .await
            .is_err()
    );
    let mut updated = original.clone().into_active_model();
    updated.version = Set(2);
    let maintenance = pg.role("probe_maintenance").await?;
    secure_update_with_scope::<entity::Entity>(
        updated.clone(),
        &exact_scope(&original).await,
        u(1),
        &maintenance.conn()?,
    )
    .await?;
    let discovery = pg.role("probe_discovery").await?;
    assert!(
        secure_update_with_scope::<entity::Entity>(updated, &scope, u(1), &discovery.conn()?)
            .await
            .is_err()
    );
    let retention = pg.role("probe_retention").await?;
    assert!(
        entity::Entity::delete_many()
            .secure()
            .scope_with(&scope)
            .exec(&retention.conn()?)
            .await
            .is_err()
    );
    assert_eq!(
        private::Entity::delete_many()
            .secure()
            .scope_with(&scope)
            .exec(&retention.conn()?)
            .await?
            .rows_affected,
        1
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_axis_change_invalidates_prepared_authorization() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let original = row(1, 10, 20, 30);
    insert(&pg.db, &original).await?;
    let broad = provider::scope(vec![provider::path(vec![provider::eq(
        "id",
        original.order_id,
    )])])
    .await;
    let barrier = tokio::sync::Barrier::new(2);
    let stale_request = async {
        let observed = rows(&pg.db, &broad).await?.remove(0);
        let mut proposed = observed.clone();
        proposed.payer_tenant_id = u(32);
        proposed.version = 2;
        let proposed_scope = exact_scope(&proposed).await;
        barrier.wait().await;
        barrier.wait().await;
        assert!(
            flows::guarded_update(&pg.db, observed, proposed, broad.clone(), proposed_scope)
                .await
                .is_err()
        );
        anyhow::Ok(())
    };
    let concurrent_request = async {
        let mut proposed = original.clone();
        proposed.payer_tenant_id = u(31);
        proposed.version = 2;
        barrier.wait().await;
        let result = flows::guarded_update(
            &pg.db,
            original.clone(),
            proposed.clone(),
            broad.clone(),
            exact_scope(&proposed).await,
        )
        .await;
        barrier.wait().await;
        result
    };
    let (stale, winner) = tokio::join!(stale_request, concurrent_request);
    stale?;
    winner?;
    let persisted = rows(&pg.db, &broad).await?.remove(0);
    assert_eq!(persisted.payer_tenant_id, u(31));
    assert_eq!(persisted.version, 2);
    Ok(())
}

#[path = "conformance_support/faults.rs"]
mod faults;
#[path = "conformance_support/pg_faults.rs"]
mod pg_faults;
