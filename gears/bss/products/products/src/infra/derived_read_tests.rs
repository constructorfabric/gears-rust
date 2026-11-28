//! The `ClientHub` read of a stored derived usage declaration (P-D-277): the read through the hub after the gear's
//! init, `sku:read` in the named catalog tenant, the REST version read's digest and refusals.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::LocalDerivedUsageTypeRead;
use crate::api::rest::{ApiState, derived_usage_types};
use crate::domain::derived::{NewDerivedType, NewDerivedVersion, digest_hex};
use crate::infra::storage::repo::derived_usage_type_repo as store;
use crate::test_support::{
    Accepting, TestDsn, authed_ctx, body_json, counting_flat_in_enforcer, get, post,
    resolved_usage_types, rest_app_on_db, test_db,
};
use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext, TenantMode,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use bss_products_sdk::DerivedUsageTypeReadV1;
use bss_products_sdk::derived::{DerivedUsageDeclaration, MeterId};
use bss_products_sdk::derived_read::StoredDerivedDeclaration;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use toolkit::api::canonical_prelude::{CanonicalError, Problem};
use toolkit_db::secure::AccessScope;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

const BASE: &str = "/bss-products/v1/derived-usage-types";
const RAM_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.ram_mb.v1";
const CPU_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.cpu_mhz.v1";

/// The cloudlet of P-D-230 on the wire.
fn cloudlet() -> Value {
    let share = |name: &str, divisor: &str| json!({"op":"ceil","arg":{"op":"div_const","arg":{"op":"input","name":name},"divisor":divisor}});
    json!({
        "output_unit": "cloudlet\u{b7}hour",
        "granularity": "hour",
        "inputs": [
            {"name":"ram_mb","usage_type_ref":RAM_REF,"granule_fold":"peak","unit":"MB"},
            {"name":"cpu_mhz","usage_type_ref":CPU_REF,"granule_fold":"peak","unit":"MHz"}
        ],
        "formula": {"op":"max","args":[share("ram_mb","128"), share("cpu_mhz","400")]},
        "output_scale": 0,
        "output_round": "half_even"
    })
}

fn meter(code: &str, n: u32) -> MeterId {
    MeterId::new(code, n).unwrap()
}

/// The context root and the tenant mode of one PDP request.
type Asked = (Option<Uuid>, Option<TenantMode>);

/// A PDP that grants `sku:read` in the tenants of `granted` only, as the platform's PDP judges a named root in
/// `root_only` mode: it answers `In(owner_tenant_id, [root])` for a granted root and denies any other. It records the
/// root and the mode of each request.
struct Granting {
    granted: Vec<Uuid>,
    asked: Mutex<Vec<Asked>>,
}
impl Granting {
    fn new(granted: &[Uuid]) -> Arc<Self> {
        Arc::new(Self {
            granted: granted.to_vec(),
            asked: Mutex::new(Vec::new()),
        })
    }
}
#[async_trait]
impl AuthZResolverApi for Granting {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let context = request.context.tenant_context;
        let root = context.as_ref().and_then(|tc| tc.root_id);
        self.asked
            .lock()
            .unwrap()
            .push((root, context.map(|tc| tc.mode)));
        let granted = root.filter(|root| self.granted.contains(root));
        Ok(EvaluationResponse {
            decision: granted.is_some(),
            context: EvaluationResponseContext {
                constraints: granted
                    .map(|root| Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            vec![root],
                        ))],
                    })
                    .into_iter()
                    .collect(),
                deny_reason: None,
            },
        })
    }
}

/// A PDP that cannot be reached.
struct Unreachable;
#[async_trait]
impl AuthZResolverApi for Unreachable {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err(CanonicalError::service_unavailable().create())
    }
}

/// A refusal's status and its whole problem body.
fn refusal(error: CanonicalError) -> (u16, Value) {
    let status = error.status_code();
    (status, serde_json::to_value(Problem::from(error)).unwrap())
}

/// Seed `code` of `tenant` through the repository, one version per `(declaration, stored digest)`.
async fn seed(dsn: &str, tenant: Uuid, code: &str, versions: &[(Value, String)]) {
    let (db, _) = crate::test_support::repo_connection(dsn, tenant).await;
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant);
    let now = time::OffsetDateTime::now_utc();
    let t = store::create_type(
        &conn,
        &scope,
        tenant,
        NewDerivedType {
            code: code.to_owned(),
            name: format!("{code} name"),
        },
        Uuid::from_u128(7),
        now,
    )
    .await
    .unwrap();
    for (n, (declaration, digest)) in (1..).zip(versions) {
        store::insert_version(
            &conn,
            &scope,
            tenant,
            NewDerivedVersion {
                type_id: t.id,
                version: n,
                declaration_json: declaration.clone(),
                digest: digest.clone(),
                created_by: Uuid::from_u128(7),
                created_at: now,
            },
        )
        .await
        .unwrap();
    }
}

/// A seller tenant whose catalog holds the cloudlet, created through the REST endpoint, and a service homed at the
/// platform root that reads it.
struct F {
    app: axum::Router,
    state: Arc<ApiState>,
    dsn: TestDsn,
    seller: Uuid,
    service: SecurityContext,
}

impl F {
    async fn new() -> Self {
        let (db, _, _, dsn) = test_db().await;
        let seller = Uuid::new_v4();
        let (app, state) = rest_app_on_db(
            seller,
            derived_usage_types::router,
            resolved_usage_types(),
            "test",
            db,
        )
        .await;
        let created = post(
            &app,
            seller,
            BASE,
            json!({"code": "cloudlets", "name": "Cloudlets", "declaration": cloudlet()}),
        )
        .await;
        assert_eq!(created.status(), 201);
        Self {
            app,
            state,
            dsn,
            seller,
            service: authed_ctx(Uuid::new_v4()),
        }
    }

    fn reader(&self, resolver: Arc<dyn AuthZResolverApi>) -> LocalDerivedUsageTypeRead {
        LocalDerivedUsageTypeRead::new(self.state.clone(), Arc::new(PolicyEnforcer::new(resolver)))
    }

    /// The read under a grant of `sku:read` in the seller tenant alone.
    async fn read(
        &self,
        catalog_tenant: Uuid,
        meter: &MeterId,
    ) -> Result<StoredDerivedDeclaration, CanonicalError> {
        self.reader(Granting::new(&[self.seller]))
            .declaration(&self.service, catalog_tenant, meter)
            .await
    }
}

/// A service homed at the root reads the seller's stored version with `sku:read` in the seller tenant: the PDP is
/// asked `root_only` with the seller tenant as the root. The answer carries the REST version read's stored digest,
/// unit and accrual, and the declaration the REST read serves; the digest is the one its canonical bytes give.
#[tokio::test]
async fn a_root_service_reads_the_sellers_declaration_with_the_rest_digest() {
    let f = F::new().await;
    let pdp = Granting::new(&[f.seller]);
    let answer = f
        .reader(pdp.clone())
        .declaration(&f.service, f.seller, &meter("cloudlets", 1))
        .await
        .unwrap();
    assert_eq!(
        *pdp.asked.lock().unwrap(),
        vec![(Some(f.seller), Some(TenantMode::RootOnly))]
    );
    let rest =
        body_json(get(&f.app, f.seller, &format!("{BASE}/cloudlets/versions/1")).await).await;
    assert_eq!(answer.meter, meter("cloudlets", 1));
    assert_eq!(Value::String(answer.digest.clone()), rest["digest"]);
    assert_eq!(
        Value::String(answer.canonical_unit.clone()),
        rest["canonical_unit"]
    );
    assert_eq!(
        Value::String(answer.accrual_policy_version.clone()),
        rest["accrual_policy_version"]
    );
    let served: crate::api::rest::dto::ProductsDerivedDeclaration =
        serde_json::from_value(rest["declaration"].clone()).unwrap();
    assert_eq!(
        answer.declaration,
        DerivedUsageDeclaration::try_from(&served).unwrap()
    );
    assert_eq!(digest_hex(&answer.declaration), answer.digest);
    assert_eq!(answer.canonical_unit, "cloudlet\u{b7}hour");
    assert_eq!(
        answer.accrual_policy_version,
        format!("derived-v1:{}", answer.digest)
    );
}

/// A catalog tenant the caller holds no `sku:read` in is 403, also when that tenant holds the meter. A grant of the
/// caller's own tenant does not reach the named one: a flat-`In` decision of the caller's tenant fails the
/// membership check.
#[tokio::test]
async fn a_tenant_without_the_grant_is_refused() {
    let f = F::new().await;
    let foreign = Uuid::new_v4();
    seed(
        &f.dsn,
        foreign,
        "cloudlets",
        &[(cloudlet(), "c".repeat(64))],
    )
    .await;
    let denied = f.read(foreign, &meter("cloudlets", 1)).await.unwrap_err();
    assert_eq!(denied.status_code(), 403, "{denied:?}");
    let own = f
        .reader(Arc::new(crate::test_support::Spanning {
            tenants: vec![f.service.subject_tenant_id()],
        }))
        .declaration(&f.service, f.seller, &meter("cloudlets", 1))
        .await
        .unwrap_err();
    assert_eq!(own.status_code(), 403, "{own:?}");
    // The seller itself, granted its own tenant, reads it.
    let seller = authed_ctx(f.seller);
    let (enforcer, _) = counting_flat_in_enforcer(f.seller);
    LocalDerivedUsageTypeRead::new(f.state.clone(), Arc::new(enforcer))
        .declaration(&seller, f.seller, &meter("cloudlets", 1))
        .await
        .unwrap();
}

/// An unknown code and an unknown version are the version endpoint's 404, one body for both.
#[tokio::test]
async fn an_unknown_meter_is_not_found() {
    let f = F::new().await;
    let unknown_code = refusal(f.read(f.seller, &meter("nothing", 1)).await.unwrap_err());
    let unknown_version = refusal(f.read(f.seller, &meter("cloudlets", 2)).await.unwrap_err());
    assert_eq!(unknown_code.0, 404, "{unknown_code:?}");
    assert_eq!(unknown_code, unknown_version);
    let endpoint = get(&f.app, f.seller, &format!("{BASE}/nothing/versions/1")).await;
    assert_eq!(endpoint.status(), 404);
}

/// A nil subject, subject tenant or catalog tenant is 403 before the PDP is asked.
#[tokio::test]
async fn a_nil_caller_or_catalog_tenant_is_403_before_the_pdp() {
    let f = F::new().await;
    let (enforcer, asked) = counting_flat_in_enforcer(f.seller);
    let reader = LocalDerivedUsageTypeRead::new(f.state.clone(), Arc::new(enforcer));
    let caller = |subject: Uuid, tenant: Uuid| {
        SecurityContext::builder()
            .subject_id(subject)
            .subject_tenant_id(tenant)
            .subject_type("gts.cf.core.security.subject_user.v1~")
            .build()
            .unwrap()
    };
    for (ctx, catalog_tenant) in [
        (caller(Uuid::now_v7(), Uuid::nil()), f.seller),
        (caller(Uuid::nil(), f.seller), f.seller),
        (authed_ctx(f.seller), Uuid::nil()),
    ] {
        let error = reader
            .declaration(&ctx, catalog_tenant, &meter("cloudlets", 1))
            .await
            .unwrap_err();
        assert_eq!(error.status_code(), 403, "{error:?}");
    }
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 0);
}

/// An unreachable PDP is 503; a stored declaration that does not read is the endpoint's data-loss 500.
#[tokio::test]
async fn an_unreachable_pdp_is_503_and_a_corrupt_row_500() {
    let f = F::new().await;
    let unreachable = f
        .reader(Arc::new(Unreachable))
        .declaration(&f.service, f.seller, &meter("cloudlets", 1))
        .await
        .unwrap_err();
    assert_eq!(unreachable.status_code(), 503, "{unreachable:?}");
    seed(
        &f.dsn,
        f.seller,
        "corrupt",
        &[(json!({"output_unit": 7}), "d".repeat(64))],
    )
    .await;
    let (status, body) = refusal(f.read(f.seller, &meter("corrupt", 1)).await.unwrap_err());
    assert_eq!(status, 500);
    assert!(
        body.to_string()
            .contains("a stored derived meter row does not read"),
        "{body}"
    );
    let endpoint = get(&f.app, f.seller, &format!("{BASE}/corrupt/versions/1")).await;
    assert_eq!(endpoint.status(), 500);
    assert!(
        body_json(endpoint)
            .await
            .to_string()
            .contains("a stored derived meter row does not read")
    );
}

/// After the gear's own init the hub's one `dyn DerivedUsageTypeReadV1` is this read over the gear's database: a
/// service homed at the root reads a seller's stored version through it. Before the init nothing answers.
#[tokio::test]
async fn the_hub_answers_the_read_after_the_gears_init() {
    use toolkit::Gear;
    use toolkit::contracts::DatabaseCapability;
    struct NoConfig;
    impl toolkit::config::ConfigProvider for NoConfig {
        fn get_gear_config(&self, _gear: &str) -> Option<&Value> {
            None
        }
    }
    let gear = crate::gear::BssProductsGear::default();
    let dsn = TestDsn::new("products-derived-read-init-");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(4),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(&db, gear.migrations())
        .await
        .unwrap();
    let seller = Uuid::new_v4();
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register::<dyn AuthZResolverApi>(Granting::new(&[seller]));
    hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(Accepting));
    assert!(hub.get::<dyn DerivedUsageTypeReadV1>().is_err());
    let ctx = toolkit::GearCtx::new(
        "bss-products",
        Uuid::new_v4(),
        Arc::new(NoConfig),
        hub.clone(),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db(toolkit_db::DBProvider::new(db));
    gear.init(&ctx).await.unwrap();
    let digest = "a".repeat(64);
    seed(&dsn, seller, "cloudlets", &[(cloudlet(), digest.clone())]).await;
    let read = hub.get::<dyn DerivedUsageTypeReadV1>().unwrap();
    let answer = read
        .declaration(&authed_ctx(Uuid::new_v4()), seller, &meter("cloudlets", 1))
        .await
        .unwrap();
    assert_eq!(answer.digest, digest, "the STORED digest, never recomputed");
    assert_eq!(
        answer.accrual_policy_version,
        format!("derived-v1:{digest}")
    );
    assert_eq!(answer.canonical_unit, "cloudlet\u{b7}hour");
    let other = read
        .declaration(
            &authed_ctx(Uuid::new_v4()),
            Uuid::new_v4(),
            &meter("cloudlets", 1),
        )
        .await
        .unwrap_err();
    assert_eq!(other.status_code(), 403, "{other:?}");
}
