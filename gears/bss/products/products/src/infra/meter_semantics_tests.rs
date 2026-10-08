//! The E1b provider (P-D-233): decision 5's answer for a derived meter, the unconfigured answer for a raw one, every
//! refusal, the tenant pin, and the hub's one registration after the gear's init.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::{METER_POLICY_MISMATCH, METER_VERSION_UNKNOWN, ProductsMeterSemantics};
use crate::api::rest::ApiState;
use crate::domain::derived::{NewDerivedType, NewDerivedVersion};
use crate::infra::storage::repo::derived_usage_type_repo as store;
use crate::test_support::{
    TestDsn, authed_ctx, counting_flat_in_enforcer, denying_enforcer, drop_table, flat_in_enforcer,
    problem_code, resolved_usage_types, rest_app_on_db, test_db,
};
use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use bss_pricing_sdk::meter_semantics::{
    MeterSemantics, UnconfiguredMeterSemantics, UsageMeterSemanticsV1,
};
use bss_pricing_sdk::terms::{Fold, MeterRef};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

const CLOUDLET_UNIT: &str = "cloudlet\u{b7}hour";
const RAM_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.ram_mb.v1";
const CPU_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.cpu_mhz.v1";

/// A permitting PDP whose compiled scope is `In(tenants)` over the owner tenant: one tenant is the flat-`In` shape a
/// tenant's grant compiles to, several the shape of a grant over a parent and its children.
struct Spanning {
    tenants: Vec<Uuid>,
}
#[async_trait]
impl AuthZResolverApi for Spanning {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        self.tenants.clone(),
                    ))],
                }],
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

/// A stored digest that no declaration hashes to: an answer that recomputed the digest would differ from it.
fn stored_digest(fill: char) -> String {
    std::iter::repeat_n(fill, 64).collect()
}

/// The cloudlet of decision 1 on the wire, selling `unit`.
fn cloudlet(unit: &str) -> Value {
    let share = |name: &str, divisor: &str| json!({"op":"ceil","arg":{"op":"div_const","arg":{"op":"input","name":name},"divisor":divisor}});
    json!({
        "output_unit": unit,
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

/// `products.derived/<code>@<n>` with version `<n>`, as pricing names it.
fn meter(code: &str, n: u32) -> MeterRef {
    MeterRef {
        usage_type_id: format!("products.derived/{code}@{n}"),
        version: n.to_string(),
    }
}

/// The 32 bytes a 64-digit hex text spells.
fn bytes(hex: &str) -> [u8; 32] {
    let mut out = [0_u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

/// Decision 5's answer for `meter`, selling `unit`, its version storing `digest`.
fn answer(meter: MeterRef, unit: &str, digest: &str) -> MeterSemantics {
    MeterSemantics {
        meter,
        canonical_unit: unit.to_owned(),
        fold: Fold::Sum,
        accrual_policy_version: format!("derived-v1:{digest}"),
        source_integrated: true,
        digest: bytes(digest),
    }
}

/// The status and the machine code of a refusal.
fn refusal(error: CanonicalError) -> (u16, String) {
    let status = error.status_code();
    let problem = toolkit::api::canonical_prelude::Problem::from(error);
    (
        status,
        problem_code(&serde_json::to_value(problem).unwrap()),
    )
}

/// A refusal as pricing would carry it on: the whole problem body.
fn problem(error: CanonicalError) -> Value {
    serde_json::to_value(toolkit::api::canonical_prelude::Problem::from(error)).unwrap()
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

fn no_routes(_: Arc<ApiState>, _: &dyn toolkit::api::OpenApiRegistry) -> axum::Router {
    axum::Router::new()
}

struct F {
    state: Arc<ApiState>,
    /// Holds the outbox the state's sink writes to.
    _app: axum::Router,
    dsn: TestDsn,
    tenant: Uuid,
    ctx: SecurityContext,
}

impl F {
    /// The tenant holds `cloudlets` @1 (`cloudlet·hour`, stored digest `a…a`) and @2 (`cloudlet·day`, `b…b`).
    async fn new() -> Self {
        let (db, _, _, dsn) = test_db().await;
        let tenant = Uuid::new_v4();
        let (app, state) =
            rest_app_on_db(tenant, no_routes, resolved_usage_types(), "test", db).await;
        seed(
            &dsn,
            tenant,
            "cloudlets",
            &[
                (cloudlet(CLOUDLET_UNIT), stored_digest('a')),
                (cloudlet("cloudlet\u{b7}day"), stored_digest('b')),
            ],
        )
        .await;
        Self {
            state,
            _app: app,
            dsn,
            tenant,
            ctx: authed_ctx(tenant),
        }
    }

    fn provider(&self, enforcer: PolicyEnforcer) -> ProductsMeterSemantics {
        ProductsMeterSemantics::new(self.state.clone(), Arc::new(enforcer))
    }

    /// The provider under a grant of the tenant alone.
    fn granted(&self) -> ProductsMeterSemantics {
        self.provider(flat_in_enforcer(self.tenant))
    }

    async fn resolve(&self, meter: MeterRef) -> Result<MeterSemantics, CanonicalError> {
        self.granted().resolve(&self.ctx, meter).await
    }
}

/// Decision 5: the exact version's output unit, a `Sum` fold, `derived-v1:<stored digest>`, source integrated, and
/// the STORED digest, which here no declaration hashes to. Version 2 is never answered for version 1.
#[tokio::test]
async fn a_derived_meter_answers_its_stored_version() {
    let f = F::new().await;
    assert_eq!(
        f.resolve(meter("cloudlets", 1)).await.unwrap(),
        answer(meter("cloudlets", 1), CLOUDLET_UNIT, &stored_digest('a'))
    );
    assert_eq!(
        f.resolve(meter("cloudlets", 2)).await.unwrap(),
        answer(
            meter("cloudlets", 2),
            "cloudlet\u{b7}day",
            &stored_digest('b')
        )
    );
}

/// A raw meter (E1a) answers exactly what pricing answers with no provider registered, and asks neither the PDP nor
/// the store: the PDP counts no evaluation, and a dropped table changes nothing.
#[tokio::test]
async fn a_raw_meter_answers_exactly_as_an_absent_provider() {
    let f = F::new().await;
    drop_table(&f.dsn, "products_derived_usage_type_version").await;
    let (enforcer, asked) = counting_flat_in_enforcer(f.tenant);
    let provider = f.provider(enforcer);
    for raw in [
        "vm-hours",
        RAM_REF,
        "",
        "products.derived",
        "Products.derived/cloudlets@1",
        "products.derivedx/cloudlets@1",
    ] {
        let error = provider
            .resolve(
                &f.ctx,
                MeterRef {
                    usage_type_id: raw.to_owned(),
                    version: "1".to_owned(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            problem(error),
            problem(CanonicalError::from(UnconfiguredMeterSemantics))
        );
    }
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[test]
fn a_short_or_uppercase_digest_does_not_decode() {
    assert!(super::decode_digest(&"a".repeat(63)).is_none());
    assert!(super::decode_digest(&"A".repeat(64)).is_none());
    assert!(super::decode_digest(&"ab".repeat(32)).is_some());
}

/// A `version` that is not canonical, or that disagrees with the id's `@<n>`, is 400 `METER_POLICY_MISMATCH`.
#[tokio::test]
async fn a_version_off_the_id_is_a_policy_mismatch() {
    let f = F::new().await;
    for version in ["01", "", " 1", "+1", "1.0", "v1", "2", "0"] {
        let error = f
            .resolve(MeterRef {
                usage_type_id: "products.derived/cloudlets@1".to_owned(),
                version: version.to_owned(),
            })
            .await
            .unwrap_err();
        assert_eq!(
            refusal(error),
            (400, METER_POLICY_MISMATCH.to_owned()),
            "{version:?}"
        );
    }
}

/// An unknown code, an unknown version, another tenant's type and a prefixed id that names no meter are ONE answer:
/// 400 `METER_VERSION_UNKNOWN`, the same body for each.
#[tokio::test]
async fn an_unknown_code_version_or_tenant_is_one_answer() {
    let f = F::new().await;
    seed(
        &f.dsn,
        Uuid::new_v4(),
        "foreign",
        &[(cloudlet(CLOUDLET_UNIT), stored_digest('c'))],
    )
    .await;
    let mut bodies = Vec::new();
    for asked in [
        meter("nothing", 1),
        meter("cloudlets", 3),
        meter("foreign", 1),
        MeterRef {
            usage_type_id: "products.derived/Cloudlets@1".to_owned(),
            version: "1".to_owned(),
        },
        MeterRef {
            usage_type_id: "products.derived/cloudlets@01".to_owned(),
            version: "1".to_owned(),
        },
        MeterRef {
            usage_type_id: "products.derived/".to_owned(),
            version: "1".to_owned(),
        },
    ] {
        let error = f.resolve(asked.clone()).await.unwrap_err();
        let body = problem(error.clone());
        assert_eq!(
            refusal(error),
            (400, METER_VERSION_UNKNOWN.to_owned()),
            "{asked:?}"
        );
        bodies.push(body);
    }
    assert!(bodies.windows(2).all(|w| w[0] == w[1]), "{bodies:?}");
}

/// A store failure is 503; a stored declaration that does not read is a corrupt row, 500.
#[tokio::test]
async fn a_store_failure_is_503_and_a_corrupt_row_500() {
    let f = F::new().await;
    seed(
        &f.dsn,
        f.tenant,
        "corrupt",
        &[(json!({"output_unit": 7}), stored_digest('d'))],
    )
    .await;
    let corrupt = f.resolve(meter("corrupt", 1)).await.unwrap_err();
    assert_eq!(corrupt.status_code(), 500);
    let body = problem(corrupt).to_string();
    assert!(
        body.contains("a stored derived meter row does not read"),
        "{body}"
    );
    assert!(!body.contains("output_unit"), "{body}");
    drop_table(&f.dsn, "products_derived_usage_type_version").await;
    assert_eq!(
        f.resolve(meter("cloudlets", 1))
            .await
            .unwrap_err()
            .status_code(),
        503
    );
}

/// A caller with no tenant or no subject is 403 before the PDP is asked: the pin needs a tenant to read in.
#[tokio::test]
async fn a_caller_without_a_tenant_or_a_subject_is_403() {
    let f = F::new().await;
    let (enforcer, asked) = counting_flat_in_enforcer(f.tenant);
    let provider = f.provider(enforcer);
    for (subject, tenant) in [(Uuid::now_v7(), Uuid::nil()), (Uuid::nil(), f.tenant)] {
        let caller = SecurityContext::builder()
            .subject_id(subject)
            .subject_tenant_id(tenant)
            .subject_type("gts.cf.core.security.subject_user.v1~")
            .build()
            .unwrap();
        let error = provider
            .resolve(&caller, meter("cloudlets", 1))
            .await
            .unwrap_err();
        assert_eq!(error.status_code(), 403, "{error:?}");
    }
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 0);
}

/// A caller the PDP denies `sku:read` is 403; a PDP that cannot be reached is 503.
#[tokio::test]
async fn a_denied_sku_read_is_403_and_an_unreachable_pdp_503() {
    let f = F::new().await;
    let denied = f
        .provider(denying_enforcer())
        .resolve(&f.ctx, meter("cloudlets", 1))
        .await
        .unwrap_err();
    assert_eq!(denied.status_code(), 403, "{denied:?}");
    let unreachable = f
        .provider(PolicyEnforcer::new(Arc::new(Unreachable)))
        .resolve(&f.ctx, meter("cloudlets", 1))
        .await
        .unwrap_err();
    assert_eq!(unreachable.status_code(), 503, "{unreachable:?}");
}

/// The caller's tenant is the read's key whatever the grant spans: a caller whose grant covers its own tenant and
/// another never reads the other's types, and reads its own where both hold the code.
#[tokio::test]
async fn the_callers_tenant_is_pinned() {
    let f = F::new().await;
    let other = Uuid::new_v4();
    seed(
        &f.dsn,
        other,
        "foreign",
        &[(cloudlet(CLOUDLET_UNIT), stored_digest('c'))],
    )
    .await;
    seed(
        &f.dsn,
        other,
        "cloudlets",
        &[(cloudlet("other\u{b7}hour"), stored_digest('e'))],
    )
    .await;
    let spanning = f.provider(PolicyEnforcer::new(Arc::new(Spanning {
        tenants: vec![f.tenant, other],
    })));
    let error = spanning
        .resolve(&f.ctx, meter("foreign", 1))
        .await
        .unwrap_err();
    assert_eq!(refusal(error), (400, METER_VERSION_UNKNOWN.to_owned()));
    assert_eq!(
        spanning
            .resolve(&f.ctx, meter("cloudlets", 1))
            .await
            .unwrap(),
        answer(meter("cloudlets", 1), CLOUDLET_UNIT, &stored_digest('a'))
    );
    let theirs = authed_ctx(other);
    assert_eq!(
        spanning
            .resolve(&theirs, meter("cloudlets", 1))
            .await
            .unwrap(),
        answer(
            meter("cloudlets", 1),
            "other\u{b7}hour",
            &stored_digest('e')
        )
    );
}

/// A permitting PDP whose only predicate is `resource_id`: no `owner_tenant_id`. `tenant_only` drops that constraint
/// and the read is deny-all, even when the resource id is the derived type's own id.
struct ResourceOnly {
    resource: Uuid,
}
#[async_trait]
impl AuthZResolverApi for ResourceOnly {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::RESOURCE_ID,
                        vec![self.resource],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// A constraint with no `owner_tenant_id` reads nothing: `tenant_only` is deny-all, so naming the derived type's id
/// as `resource_id` does not answer the meter.
#[tokio::test]
async fn a_constraint_with_no_owner_tenant_reads_nothing() {
    let f = F::new().await;
    let (db, _) = crate::test_support::repo_connection(&f.dsn, f.tenant).await;
    let conn = db.conn().unwrap();
    let stored = store::find_type(
        &conn,
        &AccessScope::for_tenant(f.tenant),
        f.tenant,
        "cloudlets",
    )
    .await
    .unwrap()
    .unwrap();
    let provider = f.provider(PolicyEnforcer::new(Arc::new(ResourceOnly {
        resource: stored.id,
    })));
    let error = provider
        .resolve(&f.ctx, meter("cloudlets", 1))
        .await
        .unwrap_err();
    assert_eq!(refusal(error), (400, METER_VERSION_UNKNOWN.to_owned()));
}

/// The `TypesRegistryClient` the gear's init registers its authz labels with; nothing else is asked of it.
struct Accepting;
#[async_trait]
impl types_registry_sdk::TypesRegistryClient for Accepting {
    async fn register(
        &self,
        _entities: Vec<Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, CanonicalError> {
        Ok(Vec::new())
    }
    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_type_schema(
        &self,
        _type_id: &str,
    ) -> Result<types_registry_sdk::GtsTypeSchema, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<types_registry_sdk::GtsTypeSchema, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_type_schemas(
        &self,
        _type_ids: Vec<String>,
    ) -> std::collections::HashMap<String, Result<types_registry_sdk::GtsTypeSchema, CanonicalError>>
    {
        std::collections::HashMap::new()
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _type_uuids: Vec<Uuid>,
    ) -> std::collections::HashMap<Uuid, Result<types_registry_sdk::GtsTypeSchema, CanonicalError>>
    {
        std::collections::HashMap::new()
    }
    async fn list_type_schemas(
        &self,
        _query: types_registry_sdk::TypeSchemaQuery,
    ) -> Result<Vec<types_registry_sdk::GtsTypeSchema>, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn register_instances(
        &self,
        _instances: Vec<Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_instance(
        &self,
        _id: &str,
    ) -> Result<types_registry_sdk::GtsInstance, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_instance_by_uuid(
        &self,
        _uuid: Uuid,
    ) -> Result<types_registry_sdk::GtsInstance, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> std::collections::HashMap<String, Result<types_registry_sdk::GtsInstance, CanonicalError>>
    {
        std::collections::HashMap::new()
    }
    async fn get_instances_by_uuid(
        &self,
        _uuids: Vec<Uuid>,
    ) -> std::collections::HashMap<Uuid, Result<types_registry_sdk::GtsInstance, CanonicalError>>
    {
        std::collections::HashMap::new()
    }
    async fn list_instances(
        &self,
        _query: types_registry_sdk::InstanceQuery,
    ) -> Result<Vec<types_registry_sdk::GtsInstance>, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
}

/// Decision 6: after the gear's own init, the hub's one `dyn UsageMeterSemanticsV1` is Products' dispatcher over the
/// gear's database: it answers a derived meter of that database from its store, and a raw meter as an absent provider
/// would. Before the init nothing answers.
#[tokio::test]
async fn the_hubs_meter_semantics_is_products_dispatcher_after_the_gears_init() {
    use toolkit::Gear;
    use toolkit::contracts::DatabaseCapability;
    struct NoConfig;
    impl toolkit::config::ConfigProvider for NoConfig {
        fn get_gear_config(&self, _gear: &str) -> Option<&Value> {
            None
        }
    }
    let gear = crate::gear::BssProductsGear::default();
    let dsn = TestDsn::new("products-meter-init-");
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
    let tenant = Uuid::new_v4();
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register::<dyn AuthZResolverApi>(Arc::new(Spanning {
        tenants: vec![tenant],
    }));
    hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(Accepting));
    assert!(hub.get::<dyn UsageMeterSemanticsV1>().is_err());
    let ctx = toolkit::GearCtx::new(
        "bss-products",
        Uuid::new_v4(),
        Arc::new(NoConfig),
        hub.clone(),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db(toolkit_db::DBProvider::new(db));
    gear.init(&ctx).await.unwrap();
    seed(
        &dsn,
        tenant,
        "cloudlets",
        &[(cloudlet(CLOUDLET_UNIT), stored_digest('a'))],
    )
    .await;
    let provider = hub.get::<dyn UsageMeterSemanticsV1>().unwrap();
    let caller = authed_ctx(tenant);
    assert_eq!(
        provider
            .resolve(&caller, meter("cloudlets", 1))
            .await
            .unwrap(),
        answer(meter("cloudlets", 1), CLOUDLET_UNIT, &stored_digest('a'))
    );
    let raw = provider
        .resolve(
            &caller,
            MeterRef {
                usage_type_id: "vm-hours".to_owned(),
                version: "v1".to_owned(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        problem(raw),
        problem(CanonicalError::from(UnconfiguredMeterSemantics))
    );
}
