//! Fixtures shared by the management tests: a configurable fake PDP (allow
//! tenant-constrained, deny, failing).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeSet, HashSet};

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use parking_lot::Mutex;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::model::{
    BundleId, BundleVersion, ContentLimits, Document, VersionId, VersionState,
};
use crate::domain::ports::{PortError, TypeCatalogPort};

/// How the fake PDP answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdpMode {
    /// Grants the listed actions (every action when `None`) within the
    /// subject's own tenant: constrained scopes name only that tenant, and a
    /// prefetch check passes only for content that tenant owns.
    Allow(Option<BTreeSet<String>>),
    /// `decision = false` for everything.
    Deny,
    /// The PDP call fails.
    Fail,
}

/// One request the fake PDP saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenRequest {
    pub subject: Uuid,
    pub action: String,
    pub resource_type: String,
    pub owner: Option<Uuid>,
    pub require_constraints: bool,
}

/// Configurable fake `AuthZResolverApi`.
pub struct FakePdp {
    mode: Mutex<PdpMode>,
    pub seen: Mutex<Vec<SeenRequest>>,
}

impl FakePdp {
    pub fn new(mode: PdpMode) -> Self {
        Self {
            mode: Mutex::new(mode),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub fn allow_all() -> Self {
        Self::new(PdpMode::Allow(None))
    }

    pub fn allow_only(actions: &[&str]) -> Self {
        Self::new(PdpMode::Allow(Some(
            actions.iter().map(|a| (*a).to_owned()).collect(),
        )))
    }

    pub fn set_mode(&self, mode: PdpMode) {
        *self.mode.lock() = mode;
    }
}

#[async_trait]
impl AuthZResolverApi for FakePdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let owner = request
            .resource
            .properties
            .get(pep_properties::OWNER_TENANT_ID)
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        self.seen.lock().push(SeenRequest {
            subject: request.subject.id,
            action: request.action.name.clone(),
            resource_type: request.resource.resource_type.clone(),
            owner,
            require_constraints: request.context.require_constraints,
        });
        let tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("subject tenant");
        let mode = self.mode.lock().clone();
        let granted = match mode {
            PdpMode::Fail => return Err(CanonicalError::internal("PDP unavailable").create()),
            PdpMode::Deny => false,
            PdpMode::Allow(None) => true,
            PdpMode::Allow(Some(actions)) => actions.contains(&request.action.name),
        };
        if !granted {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            });
        }
        if request.context.require_constraints {
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            [tenant],
                        ))],
                    }],
                    ..Default::default()
                },
            })
        } else {
            Ok(EvaluationResponse {
                decision: owner.is_none_or(|o| o == tenant),
                context: EvaluationResponseContext::default(),
            })
        }
    }
}

/// A caller in `tenant`.
pub fn ctx(subject: Uuid, tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}

// ---------------------------------------------------------------------------
// Content fixtures
// ---------------------------------------------------------------------------

pub const WIDGET: &str = "gts.cf.core.test.widget.v1~";
pub const UNKNOWN_TYPE: &str = "gts.cf.core.test.unknown.v1~";
pub const TEST_PATTERN: &str = "gts.cf.core.test.*";

/// Rego denying `action`.
pub fn deny_on(_name: &str, action: &str) -> String {
    format!("package p\n\ndeny if input.action == \"{action}\"\n")
}

/// Limits roomy enough for the fixtures.
pub fn limits() -> ContentLimits {
    ContentLimits {
        max_documents_per_version: 8,
        max_document_bytes: 4_096,
    }
}

/// A type catalog that knows [`WIDGET`] only.
pub struct FakeTypes;

#[async_trait]
impl TypeCatalogPort for FakeTypes {
    async fn known_types(&self, ids: &[String]) -> Result<HashSet<String>, PortError> {
        Ok(ids.iter().filter(|i| *i == WIDGET).cloned().collect())
    }
}

/// A draft version of `bundle_id` holding `documents`.
pub fn draft(bundle_id: BundleId, owner: Uuid, documents: Vec<Document>) -> BundleVersion {
    BundleVersion {
        id: VersionId(Uuid::new_v4()),
        bundle_id,
        owner_tenant_id: owner,
        ordinal: 1,
        state: VersionState::Draft,
        created_at: OffsetDateTime::now_utc(),
        activated_at: None,
        activated_by: None,
        documents,
    }
}

// ---------------------------------------------------------------------------
// Governance fixtures (assignments, local client)
// ---------------------------------------------------------------------------

pub mod governance {
    //! A fake tenant tree (hierarchy port) and a harness over
    //! in-memory `SQLite` with every migration applied, the real Rego
    //! catalog, the fake type-reference port and the fake PDP.
    //!
    //! ```text
    //! ROOT ─┬─ A ─┬─ A1
    //!       │     └─ S (self-managed) ── S1
    //!       └─ B
    //! ```

    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use authz_resolver_sdk::PolicyEnforcer;
    use policy_engine_sdk::management::{self as sdk, DocumentSpec, NewBundle, VersionContent};
    use sea_orm_migration::MigratorTrait;
    use tenant_resolver_sdk::BarrierMode;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::odata::LimitCfg;
    use toolkit_db::{ConnectOpts, Db, connect_db};
    use toolkit_security::SecurityContext;
    use uuid::Uuid;

    use super::{FakePdp, ctx};
    use super::{FakeTypes, WIDGET, deny_on, limits};
    use crate::domain::management::{
        GovernanceService, ManagementService, ManagementServiceParts, StoreSet,
    };
    use crate::domain::ports::{HierarchyPort, PortError};
    use crate::domain::validation::ContentValidator;
    use crate::infra::storage::Migrator;
    use crate::infra::storage::content_repo::{
        OrmAssignmentRepository, OrmBundleRepository, OrmVersionRepository,
    };

    pub const ROOT: Uuid = Uuid::from_u128(1);
    pub const TENANT_A: Uuid = Uuid::from_u128(0xA);
    pub const TENANT_A1: Uuid = Uuid::from_u128(0xA1);
    pub const TENANT_S: Uuid = Uuid::from_u128(0x5);
    pub const TENANT_S1: Uuid = Uuid::from_u128(0x51);
    pub const TENANT_B: Uuid = Uuid::from_u128(0xB);
    pub const UNKNOWN_TENANT: Uuid = Uuid::from_u128(0xDEAD);
    pub const ALICE: Uuid = Uuid::from_u128(0xA11CE);
    pub const SAM: Uuid = Uuid::from_u128(0x5A3);
    pub const BOB: Uuid = Uuid::from_u128(0xB0B);

    /// Alice administers tenant A.
    pub fn alice() -> SecurityContext {
        ctx(ALICE, TENANT_A)
    }

    /// Sam administers the self-managed tenant S.
    pub fn sam() -> SecurityContext {
        ctx(SAM, TENANT_S)
    }

    /// Bob administers tenant B.
    pub fn bob() -> SecurityContext {
        ctx(BOB, TENANT_B)
    }

    /// The fake tree, implementing both hierarchy ports with the tenant
    /// resolver's barrier semantics.
    pub struct FakeTree {
        parents: HashMap<Uuid, (Option<Uuid>, bool)>,
        pub fail: AtomicBool,
    }

    impl FakeTree {
        pub fn new() -> Self {
            Self {
                parents: HashMap::from([
                    (ROOT, (None, false)),
                    (TENANT_A, (Some(ROOT), false)),
                    (TENANT_A1, (Some(TENANT_A), false)),
                    (TENANT_S, (Some(TENANT_A), true)),
                    (TENANT_S1, (Some(TENANT_S), false)),
                    (TENANT_B, (Some(ROOT), false)),
                ]),
                fail: AtomicBool::new(false),
            }
        }

        fn enter(&self) -> Result<(), PortError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(PortError::Unavailable("tenant resolver down".to_owned()));
            }
            Ok(())
        }

        fn node(&self, id: Uuid) -> Result<(Option<Uuid>, bool), PortError> {
            self.parents
                .get(&id)
                .copied()
                .ok_or_else(|| PortError::NotFound(id.to_string()))
        }
    }

    #[async_trait]
    impl HierarchyPort for FakeTree {
        async fn ancestors(
            &self,
            _ctx: &SecurityContext,
            tenant: Uuid,
        ) -> Result<Vec<Uuid>, PortError> {
            self.enter()?;
            let mut chain = Vec::new();
            let mut next = Some(tenant);
            while let Some(id) = next {
                let (parent, managed) = self.node(id)?;
                chain.push(id);
                next = if managed { None } else { parent };
            }
            Ok(chain)
        }

        async fn is_reachable(
            &self,
            _ctx: &SecurityContext,
            from_tenant: Uuid,
            target_tenant: Uuid,
            barrier: BarrierMode,
        ) -> Result<bool, PortError> {
            self.enter()?;
            self.node(from_tenant)?;
            let mut current = target_tenant;
            loop {
                if current == from_tenant {
                    return Ok(true);
                }
                let (parent, managed) = self.node(current)?;
                if barrier == BarrierMode::Respect && managed {
                    return Ok(false);
                }
                match parent {
                    Some(parent) => current = parent,
                    None => return Ok(false),
                }
            }
        }
    }

    pub type Store = StoreSet<OrmBundleRepository, OrmVersionRepository, OrmAssignmentRepository>;

    pub struct Harness {
        pub db: Db,
        pub core: Arc<ManagementService<Store>>,
        pub service: Arc<GovernanceService<Store>>,
        pub tree: Arc<FakeTree>,
        pub pdp: Arc<FakePdp>,
    }

    pub async fn migrated_db() -> Db {
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts).await.unwrap();
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .map_err(|e| e.to_string())
            .unwrap();
        db
    }

    pub async fn harness_with(pdp: FakePdp) -> Harness {
        let db = migrated_db().await;
        let validator = Arc::new(ContentValidator::new(Arc::new(FakeTypes), limits()));
        let store = Arc::new(StoreSet {
            bundles: OrmBundleRepository::new(LimitCfg {
                default: 25,
                max: 100,
            }),
            versions: OrmVersionRepository,
            assignments: OrmAssignmentRepository,
        });
        let pdp = Arc::new(pdp);
        let core = Arc::new(ManagementService::new(ManagementServiceParts {
            db: db.clone(),
            store,
            enforcer: PolicyEnforcer::new(pdp.clone()),
            validator,
        }));
        let tree = Arc::new(FakeTree::new());
        let service = Arc::new(GovernanceService::new(Arc::clone(&core), tree.clone()));
        Harness {
            db,
            core,
            service,
            tree,
            pdp,
        }
    }

    pub async fn harness() -> Harness {
        harness_with(FakePdp::allow_all()).await
    }

    /// A valid document over WIDGET named `name` (denies `delete`).
    pub fn spec(name: &str) -> DocumentSpec {
        DocumentSpec {
            name: name.to_owned(),
            content: deny_on(name, "delete"),
            resource_types: vec![WIDGET.to_owned()],
            actions: Vec::new(),
        }
    }

    impl Harness {
        /// A bundle of `owner`'s context with a draft holding `names`.
        pub async fn draft(
            &self,
            owner: &SecurityContext,
            names: &[&str],
        ) -> (sdk::Bundle, sdk::BundleVersion) {
            let bundle = self
                .core
                .create_bundle(owner, NewBundle::new(format!("b-{}", Uuid::new_v4())))
                .await
                .unwrap();
            let draft = self
                .core
                .create_draft_version(owner, bundle.id, None)
                .await
                .unwrap();
            let version = self
                .core
                .replace_draft_content(
                    owner,
                    bundle.id,
                    draft.id,
                    VersionContent {
                        documents: names.iter().map(|n| spec(n)).collect(),
                    },
                )
                .await
                .unwrap()
                .version;
            (bundle, version)
        }

        /// A bundle of `owner`'s context with an active version holding
        /// `names`.
        pub async fn active(
            &self,
            owner: &SecurityContext,
            names: &[&str],
        ) -> (sdk::Bundle, sdk::BundleVersion) {
            let (bundle, draft) = self.draft(owner, names).await;
            let active = self
                .core
                .activate_version(owner, bundle.id, draft.id)
                .await
                .unwrap();
            (bundle, active)
        }
    }
}
