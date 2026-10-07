//! P-D-245: `skus_for_write` leaves out a missing or unadmitted id, and the default and Products'
//! override agree. 403 and 503 fail the whole call. The override binds the id set once.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::LocalReferenceRegistry;
use crate::domain::sku::NewSku;
use crate::infra::storage::repo;
use crate::test_support::{
    TestDsn, authed_ctx, flat_in_enforcer, recorded_test_db, resolved_usage_types, rest_app_on_db,
    test_db,
};
use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use bss_products_sdk::ReferenceRegistryV1;
use bss_products_sdk::models::{
    ReferenceKind, ReferenceState, ReservationReceipt, Sku, SkuType, SkuVersion,
};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

/// The trait's default `skus_for_write`, over the same registry the override is.
struct ThroughDefault(Arc<LocalReferenceRegistry>);

#[async_trait]
impl ReferenceRegistryV1 for ThroughDefault {
    async fn reserve(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku_id: Uuid,
        kind: ReferenceKind,
        ref_id: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError> {
        self.0.reserve(ctx, tenant, sku_id, kind, ref_id).await
    }
    async fn confirm(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.0.confirm(ctx, tenant, id).await
    }
    async fn release(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.0.release(ctx, tenant, id).await
    }
    async fn states(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError> {
        self.0.states(ctx, tenant, ids).await
    }
    async fn sku_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        self.0.sku_for_write(ctx, tenant, id).await
    }
    async fn sku_version_as_of(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError> {
        self.0.sku_version_as_of(ctx, tenant, id, date).await
    }
}

struct Policy {
    tenant: Uuid,
    /// `None`: the caller may read every SKU. `Some`: only these ids.
    admitted: Option<Vec<Uuid>>,
    deny: bool,
    down: bool,
}

#[async_trait]
impl AuthZResolverApi for Policy {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        _: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        if self.down {
            return Err(CanonicalError::service_unavailable().create());
        }
        let mut predicates = vec![Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            vec![self.tenant],
        ))];
        if let Some(ids) = &self.admitted {
            predicates.push(Predicate::In(InPredicate::new(
                pep_properties::RESOURCE_ID,
                ids.clone(),
            )));
        }
        Ok(EvaluationResponse {
            decision: !self.deny,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                deny_reason: None,
            },
        })
    }
}

struct Held {
    registry: Arc<LocalReferenceRegistry>,
    tenant: Uuid,
    ctx: SecurityContext,
    _dsn: TestDsn,
}

async fn author_scope(tenant: Uuid) -> AccessScope {
    crate::authz::access_scope(
        &flat_in_enforcer(tenant),
        &authed_ctx(tenant),
        &crate::authz::resource_types::SKU,
        crate::authz::actions::AUTHOR,
        Some(tenant),
    )
    .await
    .unwrap()
}

fn enforcer(tenant: Uuid) -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(Policy {
        tenant,
        admitted: None,
        deny: false,
        down: false,
    }))
}

async fn agree(held: &Held, asked: &[Uuid]) -> (Vec<Sku>, Vec<Sku>) {
    let via_default = ThroughDefault(held.registry.clone())
        .skus_for_write(&held.ctx, held.tenant, asked)
        .await;
    let via_override = held
        .registry
        .skus_for_write(&held.ctx, held.tenant, asked)
        .await;
    (via_default.unwrap(), via_override.unwrap())
}

/// The default and the override leave out the same missing id and agree for 1, 10 and 100.
#[tokio::test]
async fn the_default_and_the_override_agree_for_1_10_and_100_ids() {
    let (db, _, tenant, dsn) = test_db().await;
    let scope = author_scope(tenant).await;
    let mut ids = Vec::new();
    let foreign_tenant;
    {
        let conn = db.conn().unwrap();
        for i in 0..100 {
            let sku = repo::insert_sku(
                &conn,
                &scope,
                tenant,
                NewSku {
                    code: format!("sku-{i:03}"),
                    name: format!("SKU {i:03}"),
                    r#type: SkuType::Usage,
                    category_id: None,
                    description: String::new(),
                    sellable: true,
                    gl_code: None,
                    tax_category: None,
                    invoice_line_template: None,
                    billing_timing: None,
                    usage_type_ref: None,
                    unit: None,
                },
                Uuid::nil(),
                time::OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .unwrap();
            ids.push(sku.id);
        }
        let other = uuid::Uuid::new_v4();
        foreign_tenant = other;
        let other_scope = author_scope(other).await;
        let foreign = repo::insert_sku(
            &conn,
            &other_scope,
            other,
            NewSku {
                code: "foreign".into(),
                name: "Foreign".into(),
                r#type: SkuType::Usage,
                category_id: None,
                description: String::new(),
                sellable: true,
                gl_code: None,
                tax_category: None,
                invoice_line_template: None,
                billing_timing: None,
                usage_type_ref: None,
                unit: None,
            },
            Uuid::nil(),
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
        ids.push(foreign.id);
    }
    let (_, state) = rest_app_on_db(
        tenant,
        crate::api::rest::skus::router,
        resolved_usage_types(),
        "test",
        db,
    )
    .await;
    let held = Held {
        registry: Arc::new(
            LocalReferenceRegistry::for_owner("pricing")
                .with_runtime(state, Arc::new(enforcer(tenant))),
        ),
        tenant,
        ctx: authed_ctx(tenant),
        _dsn: dsn,
    };
    let missing = Uuid::now_v7();
    for n in [1, 10, 100] {
        let mut asked: Vec<Uuid> = ids[..n].iter().rev().copied().collect();
        asked.push(asked[0]);
        asked.insert(0, missing);
        let (default, over) = agree(&held, &asked).await;
        assert_eq!(default, over, "{n}");
        assert_eq!(default.len(), n, "{n}");
        assert!(default.iter().all(|sku| sku.id != missing));
        let expected: Vec<Uuid> = ids[..n].iter().rev().copied().collect();
        assert_eq!(
            default.iter().map(|sku| sku.id).collect::<Vec<_>>(),
            expected,
            "{n}"
        );
    }
    let foreign = ids[100];
    let (left_out, also) = agree(&held, &[ids[0], foreign]).await;
    assert_eq!(left_out, also);
    assert_eq!(
        left_out.iter().map(|sku| sku.id).collect::<Vec<_>>(),
        vec![ids[0]]
    );
    let mismatch = held
        .registry
        .skus_for_write(&held.ctx, foreign_tenant, &ids[..1])
        .await
        .unwrap_err();
    assert_eq!(mismatch.status_code(), 403);
    let body =
        serde_json::to_string(&toolkit_canonical_errors::Problem::from_error(&mismatch).unwrap())
            .unwrap();
    assert!(body.contains("REFERENCE_OWNER_MISMATCH"), "{body}");
}

/// An id outside the caller's products-read scope is left out by both, as a 404 would be.
#[tokio::test]
async fn an_unadmitted_id_is_left_out_by_the_default_and_the_override() {
    let (db, _, tenant, dsn) = test_db().await;
    let scope = author_scope(tenant).await;
    let mut ids = Vec::new();
    {
        let conn = db.conn().unwrap();
        for i in 0..4 {
            ids.push(
                repo::insert_sku(
                    &conn,
                    &scope,
                    tenant,
                    NewSku {
                        code: format!("n-{i}"),
                        name: format!("N {i}"),
                        r#type: SkuType::Usage,
                        category_id: None,
                        description: String::new(),
                        sellable: true,
                        gl_code: None,
                        tax_category: None,
                        invoice_line_template: None,
                        billing_timing: None,
                        usage_type_ref: None,
                        unit: None,
                    },
                    Uuid::nil(),
                    time::OffsetDateTime::UNIX_EPOCH,
                )
                .await
                .unwrap()
                .id,
            );
        }
    }
    let admitted = vec![ids[0], ids[2]];
    let (_, state) = rest_app_on_db(
        tenant,
        crate::api::rest::skus::router,
        resolved_usage_types(),
        "test",
        db,
    )
    .await;
    let held = Held {
        registry: Arc::new(LocalReferenceRegistry::for_owner("pricing").with_runtime(
            state,
            Arc::new(PolicyEnforcer::new(Arc::new(Policy {
                tenant,
                admitted: Some(admitted.clone()),
                deny: false,
                down: false,
            }))),
        )),
        tenant,
        ctx: authed_ctx(tenant),
        _dsn: dsn,
    };
    let (default, over) = agree(&held, &ids).await;
    assert_eq!(default, over);
    assert_eq!(
        default.iter().map(|sku| sku.id).collect::<Vec<_>>(),
        admitted
    );
}

/// 403 and 503 fail the whole call, on the default and on the override.
#[tokio::test]
async fn a_refused_or_unanswered_caller_fails_the_whole_batch() {
    let (db, _, tenant, dsn) = test_db().await;
    let (_, state) = rest_app_on_db(
        tenant,
        crate::api::rest::skus::router,
        resolved_usage_types(),
        "test",
        db,
    )
    .await;
    let id = Uuid::now_v7();
    for (deny, down, status) in [(true, false, 403), (false, true, 503)] {
        let registry = Arc::new(LocalReferenceRegistry::for_owner("pricing").with_runtime(
            state.clone(),
            Arc::new(PolicyEnforcer::new(Arc::new(Policy {
                tenant,
                admitted: None,
                deny,
                down,
            }))),
        ));
        let ctx = authed_ctx(tenant);
        let via_default = ThroughDefault(registry.clone())
            .skus_for_write(&ctx, tenant, &[id])
            .await
            .unwrap_err();
        let via_override = registry
            .skus_for_write(&ctx, tenant, &[id])
            .await
            .unwrap_err();
        assert_eq!(via_default.status_code(), status);
        assert_eq!(via_override.status_code(), status);
    }
    let _dsn = dsn;
}

/// P-D-245: the id set is one bind, so 1 id and 100 ids are the same statement.
#[tokio::test]
async fn the_batch_binds_the_id_set_once() {
    let (db, scope, tenant, _dsn, recorder) = recorded_test_db().await;
    let conn = db.conn().unwrap();
    let mut ids = Vec::new();
    for i in 0..100 {
        ids.push(
            repo::insert_sku(
                &conn,
                &scope,
                tenant,
                NewSku {
                    code: format!("b-{i:03}"),
                    name: format!("B {i:03}"),
                    r#type: SkuType::Usage,
                    category_id: None,
                    description: String::new(),
                    sellable: true,
                    gl_code: None,
                    tax_category: None,
                    invoice_line_template: None,
                    billing_timing: None,
                    usage_type_ref: None,
                    unit: None,
                },
                Uuid::nil(),
                time::OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .unwrap()
            .id,
        );
    }
    let backend = db.db().backend();
    let mut shapes = Vec::new();
    for n in [1, 10, 100] {
        recorder.clear();
        let found = repo::find_skus(&conn, backend, &scope, tenant, &ids[..n])
            .await
            .unwrap();
        assert_eq!(found.len(), n);
        let selects: Vec<_> = recorder
            .events()
            .into_iter()
            .filter(|q| q.sql.to_lowercase().starts_with("select"))
            .collect();
        assert_eq!(
            selects.len(),
            1,
            "{n}: {:?}",
            selects.iter().map(|q| &q.sql).collect::<Vec<_>>()
        );
        assert!(
            selects[0].raw_sql.contains("json_each"),
            "{n}: {}",
            selects[0].raw_sql
        );
        shapes.push((selects[0].sql.clone(), selects[0].param_count));
    }
    assert_eq!(shapes[0], shapes[1]);
    assert_eq!(shapes[1], shapes[2]);
}
