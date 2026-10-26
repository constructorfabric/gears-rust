//! Fixtures shared by the management tests: a configurable fake PDP (allow
//! tenant-constrained, deny, failing).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeSet, HashSet};
use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use parking_lot::Mutex;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::model::{
    BundleId, BundleVersion, ContentLimits, Document, VersionId, VersionState,
};
use crate::domain::ports::{PortError, TypeCatalogPort};

/// A one-shot action the fake PDP awaits before answering.
pub type Hook = Pin<Box<dyn Future<Output = ()> + Send>>;

/// How the fake PDP answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdpMode {
    /// Grants the listed actions (every action when `None`) within the
    /// subject's own tenant: constrained scopes name only that tenant, and a
    /// prefetch check passes only for content that tenant owns.
    Allow(Option<BTreeSet<String>>),
    /// Grants every action, constrained to the listed resource identities
    /// (whatever the request names, and even where constraints are not
    /// required).
    AllowResources(Vec<Uuid>),
    /// Answers like the `static-authz` plugin: every action granted,
    /// constrained to the subject's own tenant, whatever the request names
    /// and even where constraints are not required.
    OwnTenantConstrained,
    /// Answers like the `tr-authz` plugin for a caller administering the
    /// listed tenants: every action on content they own is granted,
    /// constrained to the requested owner (to the listed tenants for a
    /// request naming none); content of any other tenant is refused.
    Administers(Vec<Uuid>),
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
    /// Awaited once, before the next answer: a concurrent writer acting
    /// between a caller's reads.
    pub before_next: Mutex<Option<Hook>>,
}

impl FakePdp {
    pub fn new(mode: PdpMode) -> Self {
        Self {
            mode: Mutex::new(mode),
            seen: Mutex::new(Vec::new()),
            before_next: Mutex::new(None),
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
        let hook = self.before_next.lock().take();
        if let Some(hook) = hook {
            hook.await;
        }
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
        let constrained = |property: &str, ids: Vec<Uuid>| EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(property, ids))],
                }],
                ..Default::default()
            },
        };
        match &mode {
            PdpMode::AllowResources(ids) => {
                return Ok(constrained(pep_properties::RESOURCE_ID, ids.clone()));
            }
            PdpMode::OwnTenantConstrained => {
                return Ok(constrained(pep_properties::OWNER_TENANT_ID, vec![tenant]));
            }
            PdpMode::Administers(tenants) => {
                return Ok(match owner {
                    Some(owner) if tenants.contains(&owner) => {
                        constrained(pep_properties::OWNER_TENANT_ID, vec![owner])
                    }
                    Some(_) => EvaluationResponse {
                        decision: false,
                        context: EvaluationResponseContext::default(),
                    },
                    None => constrained(pep_properties::OWNER_TENANT_ID, tenants.clone()),
                });
            }
            _ => {}
        }
        let granted = match mode {
            PdpMode::Fail => return Err(CanonicalError::internal("PDP unavailable").create()),
            PdpMode::Deny => false,
            PdpMode::Allow(None)
            | PdpMode::AllowResources(_)
            | PdpMode::OwnTenantConstrained
            | PdpMode::Administers(_) => true,
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

pub use crate::test_support::ctx;

// ---------------------------------------------------------------------------
// Content fixtures
// ---------------------------------------------------------------------------

pub use crate::test_support::{TEST_PATTERN, UNKNOWN_TYPE, WIDGET};

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
    //! The tenant tree the tests use and a harness over
    //! in-memory `SQLite` with every migration applied, the real Rego
    //! catalog, the fake type-reference port and the fake PDP.
    //!
    //! ```text
    //! ROOT ─┬─ A ─┬─ A1
    //!       │     └─ S (self-managed) ── S1
    //!       └─ B
    //! ```

    use std::sync::Arc;

    use authz_resolver_sdk::PolicyEnforcer;
    use policy_engine_sdk::management::{self as sdk, DocumentSpec, NewBundle, VersionContent};
    use toolkit_db::Db;
    use toolkit_db::odata::LimitCfg;
    use toolkit_security::SecurityContext;
    use uuid::Uuid;

    use time::OffsetDateTime;
    use toolkit_db::secure::AccessScope;

    use super::{FakePdp, ctx};
    use super::{FakeTypes, WIDGET, deny_on, limits};
    use crate::domain::management::{ManagementService, ManagementServiceParts, StoreSet};
    use crate::domain::model::{Assignment, AssignmentId, BundleId, VersionId, VersionState};
    use crate::domain::repos::{AssignmentRepository, VersionRepository};
    use crate::domain::validation::ContentValidator;
    use crate::infra::storage::content_repo::{
        OrmAssignmentRepository, OrmBundleRepository, OrmVersionRepository,
    };
    pub use crate::test_support::{FakeTree, migrated_db};

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

    /// The tree above, with the tenant resolver's barrier semantics.
    pub fn tree() -> FakeTree {
        FakeTree::new(&[
            (ROOT, None, false),
            (TENANT_A, Some(ROOT), false),
            (TENANT_A1, Some(TENANT_A), false),
            (TENANT_S, Some(TENANT_A), true),
            (TENANT_S1, Some(TENANT_S), false),
            (TENANT_B, Some(ROOT), false),
        ])
    }

    pub type Store = StoreSet<OrmBundleRepository, OrmVersionRepository, OrmAssignmentRepository>;

    pub struct Harness {
        pub db: Db,
        pub service: Arc<ManagementService<Store>>,
        pub tree: Arc<FakeTree>,
        pub pdp: Arc<FakePdp>,
    }

    pub async fn harness_with(pdp: FakePdp) -> Harness {
        harness_over(pdp, tree()).await
    }

    /// A harness over `tree` instead of the tree above.
    pub async fn harness_over(pdp: FakePdp, tree: FakeTree) -> Harness {
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
        let tree = Arc::new(tree);
        let service = Arc::new(ManagementService::new(ManagementServiceParts {
            db: db.clone(),
            store,
            enforcer: PolicyEnforcer::new(pdp.clone()),
            validator,
            hierarchy: tree.clone(),
        }));
        Harness {
            db,
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
                .service
                .create_bundle(owner, NewBundle::new(format!("b-{}", Uuid::new_v4())))
                .await
                .unwrap();
            let draft = self
                .service
                .create_draft_version(owner, bundle.id, None)
                .await
                .unwrap();
            let version = self
                .service
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
                .service
                .activate_version(owner, bundle.id, draft.id)
                .await
                .unwrap();
            (bundle, active)
        }

        /// An assignment of `bundle_id` made by `owner` at `tenant`, stored
        /// directly - as one made before a barrier was raised would be.
        pub async fn stored_by(
            &self,
            bundle_id: BundleId,
            owner: Uuid,
            tenant: Uuid,
        ) -> Assignment {
            let conn = self.db.conn().unwrap();
            let at = OffsetDateTime::now_utc();
            OrmAssignmentRepository
                .insert(
                    &conn,
                    &AccessScope::allow_all(),
                    &Assignment {
                        id: AssignmentId(Uuid::new_v4()),
                        bundle_id,
                        tenant_id: tenant,
                        owner_tenant_id: owner,
                        enforce: true,
                        created_at: at,
                        created_by: ALICE,
                        updated_at: at,
                        updated_by: ALICE,
                    },
                )
                .await
                .unwrap()
        }

        /// The stored assignment, read under the gear's own scope.
        pub async fn stored_assignment(&self, id: AssignmentId) -> Option<Assignment> {
            let conn = self.db.conn().unwrap();
            OrmAssignmentRepository
                .get(&conn, &AccessScope::allow_all(), id)
                .await
                .unwrap()
        }

        /// The version's stored state, read under the gear's own scope.
        pub async fn stored_state(&self, id: VersionId) -> VersionState {
            let conn = self.db.conn().unwrap();
            OrmVersionRepository
                .get_with_content(&conn, &AccessScope::allow_all(), id)
                .await
                .unwrap()
                .expect("version exists")
                .state
        }
    }
}
