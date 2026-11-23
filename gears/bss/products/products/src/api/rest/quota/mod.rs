//! The quota catalog's endpoints (P-D-266 … P-D-273), under `/bss-products/v1`, with what they
//! share: the REST state, types-registry and the discovery resolver, and the intake's settings.
//! Every endpoint authorizes before it reads the body and asks the PDP for the caller's tenant
//! only (P-D-265); every POST takes an optional `Idempotency-Key` (P-D-198).
use std::collections::HashMap;
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use axum::extract::rejection::JsonRejection;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::{Extension, Json, Router};
use bss_products_sdk::quota::catalog::QuotaVersion;
use serde::de::DeserializeOwned;
use serde_json::Value;
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::DbConn;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::{ApiState, TxError, replay, repo_error_to_canonical, tx_to_canonical_on};
use crate::authz::{access_scope, actions, labels, resource_types};
use crate::domain::error::DomainError;
use crate::domain::quota::QuotaCandidate;
use crate::domain::quota::ports::{QuotaDiscoveryResolver, QuotaRegistry};
use crate::domain::quota::snapshot::QuotaSettings;
use crate::infra::error_mapping::{QuotaResource, quota_data_loss, quota_refusal};
use crate::infra::idempotency::IdempotencyClaimInput;
use crate::infra::storage::RepoError;
use crate::infra::storage::repo::{SkuListError, quota_catalog_repo};
use dto::{ProductsQuotaCandidate, ProductsQuotaOwnerRef};
use toolkit_db::secure::{AccessScope, DBRunner};

pub mod candidates;
pub mod definitions;
pub mod discovery;
pub mod dto;
pub mod intake;
pub mod review;
pub mod targets;

// The suites sit beside the module, so their paths read `api::rest::quota::<suite>::…`, as the
// intake definition of done names them.
#[cfg(test)]
#[path = "candidates_tests.rs"]
mod candidates_tests;
#[cfg(test)]
#[path = "definitions_tests.rs"]
mod definitions_tests;
#[cfg(test)]
#[path = "discovery_tests.rs"]
mod discovery_tests;
#[cfg(test)]
#[path = "dto_tests.rs"]
mod dto_tests;
#[cfg(test)]
#[path = "review_tests.rs"]
mod review_tests;
#[cfg(test)]
#[path = "targets_tests.rs"]
mod targets_tests;

/// What the quota endpoints share (P-D-268 … P-D-272). `ApiState` stays as it is.
pub struct QuotaState {
    pub api: Arc<ApiState>,
    pub registry: Arc<dyn QuotaRegistry>,
    pub discovery: Arc<dyn QuotaDiscoveryResolver>,
    pub settings: QuotaSettings,
    /// Where an approval's transaction waits just before it moves the candidate: the Postgres
    /// race suite releases two approvals together there. `None` everywhere else.
    #[doc(hidden)]
    pub approve_hook: Option<Arc<tokio::sync::Barrier>>,
}

/// The adapter targets' path.
pub(crate) const TARGETS: &str = "/bss-products/v1/quota-adapter-targets";
/// The candidates' path.
pub(crate) const CANDIDATES: &str = "/bss-products/v1/quota-candidates";

/// types-registry does not answer (P-D-268).
pub const QUOTA_REGISTRY_UNAVAILABLE: &str = "QUOTA_REGISTRY_UNAVAILABLE";

/// Register the quota operations, with their shared state; the gear mounts them. The discovery
/// operation's request timeout comes from the state's discovery deadline (P-D-268). Public for
/// the Postgres suites only, which mount them over their own database.
#[doc(hidden)]
pub fn router(state: Arc<QuotaState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = targets::register(Router::new(), openapi);
    let router = discovery::register(router, openapi, state.settings.deadline);
    let router = candidates::register(router, openapi);
    let router = review::register(router, openapi);
    let router = definitions::register(router, openapi);
    router.layer(Extension(state))
}

/// Stored candidates as the endpoints answer them: an approved one names its definition version
/// with the definition's stored digest, read for all of `candidates` in one statement under
/// `scope`.
///
/// # Errors
/// Storage failures; an approved candidate whose definition version is not stored is a corrupt
/// row.
pub(crate) async fn candidate_dtos(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    candidates: Vec<QuotaCandidate>,
) -> Result<Vec<ProductsQuotaCandidate>, RepoError> {
    let keys: Vec<(Uuid, QuotaVersion)> = candidates
        .iter()
        .filter_map(|c| c.definition_id.zip(c.definition_version))
        .collect();
    let digests = if keys.is_empty() {
        HashMap::new()
    } else {
        quota_catalog_repo::definition_digests(runner, scope, tenant, &keys).await?
    };
    candidates
        .into_iter()
        .map(|candidate| {
            let definition_ref = match candidate.definition_id.zip(candidate.definition_version) {
                Some((id, version)) => {
                    let digest = digests.get(&(id, version)).cloned().ok_or_else(|| {
                        RepoError::CorruptRow(format!(
                            "a stored quota row does not read: products_quota_candidate, tenant \
                             {tenant}, id {}: its definition {id} version {version} is not stored",
                            candidate.id
                        ))
                    })?;
                    Some(ProductsQuotaOwnerRef {
                        id,
                        version: version.to_string(),
                        digest,
                    })
                }
                None => None,
            };
            Ok(ProductsQuotaCandidate::of(candidate, definition_ref))
        })
        .collect()
}

/// [`candidate_dtos`] of one candidate.
///
/// # Errors
/// As [`candidate_dtos`].
pub(crate) async fn candidate_dto(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    candidate: QuotaCandidate,
) -> Result<ProductsQuotaCandidate, RepoError> {
    let mut answered = candidate_dtos(runner, scope, tenant, vec![candidate]).await?;
    answered
        .pop()
        .ok_or_else(|| RepoError::Db("a candidate did not answer".into()))
}

/// The quota resource an endpoint answers its refusals on (P-D-273): the target endpoints and the
/// discovery on the adapter target; the candidate, review and definition endpoints on the quota
/// definition. The endpoint decides, not the code, so a generic refusal never names the SKU.
#[derive(Clone, Copy, Debug)]
pub(crate) struct On(QuotaResource);

impl On {
    /// The adapter target endpoints and the discovery.
    pub(crate) const TARGET: Self = Self(QuotaResource::AdapterTarget);
    /// The candidate, review and definition endpoints.
    pub(crate) const DEFINITION: Self = Self(QuotaResource::Definition);

    /// A refusal on this resource; a miss names the quota row that is missing.
    pub(crate) fn refuse(self, err: DomainError) -> CanonicalError {
        quota_refusal(err, self.0)
    }

    /// A transaction's failure, after its retry loop, on this resource.
    pub(crate) fn tx(self, err: TxError) -> CanonicalError {
        match err {
            TxError::Repo(e @ RepoError::CorruptRow(_)) => self.stored(&e),
            other => tx_to_canonical_on(other, |d| self.refuse(d)),
        }
    }

    /// A stored quota row that does not read is a data-loss 500 whose wire detail is the fixed
    /// sentence, never a miss and never skipped (P-D-274); the row's table, tenant and id stay in
    /// the log. Any other repository failure keeps the shared mapping.
    pub(crate) fn stored(self, err: &RepoError) -> CanonicalError {
        if matches!(err, RepoError::CorruptRow(_)) {
            tracing::error!(error = %err, "{STORED_ROW_DOES_NOT_READ}");
            return quota_data_loss(self.0, STORED_ROW_DOES_NOT_READ);
        }
        repo_error_to_canonical(err)
    }

    /// A list's failure: a cursor or query the pager refuses is 400, a storage failure as stored
    /// rows fail.
    pub(crate) fn list(self, err: SkuListError) -> CanonicalError {
        match err {
            SkuListError::Query(e) => CanonicalError::from(e),
            SkuListError::Repo(e) => self.stored(&e),
        }
    }

    /// A connection to read with.
    pub(crate) fn conn(self, api: &ApiState) -> Result<DbConn<'_>, CanonicalError> {
        api.db.conn().map_err(|e| self.tx(e.into()))
    }

    /// The JSON body; one that does not read is 400 `VALIDATION` on `body`, and one holding
    /// U+0000 in any string, value or key, 400 `VALIDATION` on its path (P-D-273): Postgres
    /// refuses the character in text and `jsonb` while `SQLite` stores it, so neither engine ever
    /// sees one.
    pub(crate) fn body(
        self,
        body: Result<Json<Value>, JsonRejection>,
    ) -> Result<Value, CanonicalError> {
        let body = body.map(|Json(body)| body).map_err(|error| {
            self.refuse(crate::api::rest::governance::validation(
                "body",
                error.body_text(),
            ))
        })?;
        match nul_at(&body, None) {
            None => Ok(body),
            Some(path) => Err(self.refuse(crate::api::rest::governance::validation(
                &path,
                "text must not contain a NUL character (\\u0000)",
            ))),
        }
    }

    /// The body as its request type; one that does not read is 400 `VALIDATION` on `body`.
    pub(crate) fn read<T: DeserializeOwned>(self, payload: Value) -> Result<T, CanonicalError> {
        serde_json::from_value(payload).map_err(|e| {
            self.refuse(crate::api::rest::governance::validation(
                "body",
                e.to_string(),
            ))
        })
    }

    /// The claim of a keyed request (P-D-198); a key the header cannot carry is 400 here.
    pub(crate) fn input(
        self,
        api: &ApiState,
        headers: &HeaderMap,
        endpoint: String,
        payload: &Value,
    ) -> Result<Option<IdempotencyClaimInput>, CanonicalError> {
        replay::input(api, headers, endpoint, payload).map_err(|e| self.refuse(e))
    }

    /// A keyed write's stored answer, read before anything is resolved or judged (P-D-198).
    pub(crate) async fn replayed(
        self,
        api: &ApiState,
        tenant: Uuid,
        claim: Option<&IdempotencyClaimInput>,
    ) -> Result<Option<Response>, CanonicalError> {
        replay::lookup(&self.conn(api)?, tenant, claim)
            .await
            .map_err(|e| self.tx(e))
    }
}

/// The path of the first string, value or key, of a body that holds U+0000: `body` for the body
/// itself, `<key>`, `<path>.<key>` and `<path>[i]` below it, and `<path> (a key)` for a key. A body
/// is parsed by `serde_json`, whose depth limit bounds the walk.
fn nul_at(value: &Value, path: Option<&str>) -> Option<String> {
    let here = || path.unwrap_or("body").to_owned();
    match value {
        Value::String(text) => text.contains('\0').then(here),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(i, item)| nul_at(item, Some(&format!("{}[{i}]", here())))),
        Value::Object(fields) => fields.iter().find_map(|(key, item)| {
            if key.contains('\0') {
                return Some(format!("{} (a key)", here()));
            }
            let below = path.map_or_else(|| key.clone(), |path| format!("{path}.{key}"));
            nul_at(item, Some(&below))
        }),
        _ => None,
    }
}

/// `action` on `quota_definition` as the PDP grants it, anchored to the caller's tenant for a
/// write.
async fn definition_grant(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    action: &str,
) -> Result<AccessScope, CanonicalError> {
    let anchor = (action != actions::READ).then(|| ctx.subject_tenant_id());
    access_scope(
        enforcer,
        ctx,
        &resource_types::QUOTA_DEFINITION,
        action,
        anchor,
    )
    .await
    .map_err(|e| {
        crate::api::rest::authz_error_to_canonical(e, |reason| {
            crate::infra::error_mapping::permission_denied(labels::QUOTA_DEFINITION, reason)
        })
    })
}

/// `action` on `quota_definition` for the rows that are not definitions: a candidate's reads and
/// review writes, and an approval's writes, keep only the grant's tenant (`tenant_only`), since a
/// row constraint of the grant names definitions, not those rows.
pub(crate) async fn definition_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    action: &str,
) -> Result<AccessScope, CanonicalError> {
    definition_grant(enforcer, ctx, action)
        .await
        .map(|scope| scope.tenant_only())
}

/// `action` on `quota_definition` for a review write, from one PDP question (P-D-273): the whole
/// grant, its row constraints included, under which the definition a proposal names is read, and
/// its `tenant_only()` for the candidate, the mapping, the availability and the inserts.
pub(crate) async fn review_scopes(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    action: &str,
) -> Result<(AccessScope, AccessScope), CanonicalError> {
    let grant = definition_grant(enforcer, ctx, action).await?;
    let rows = grant.tenant_only();
    Ok((grant, rows))
}

/// `read` on `quota_definition` for the definition rows themselves (P-D-273): the whole grant,
/// its row constraints included, so a grant on some definitions lists and reads only those.
pub(crate) async fn definition_read_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<AccessScope, CanonicalError> {
    definition_grant(enforcer, ctx, actions::READ).await
}

/// The wire detail and the log line of a stored quota row that does not read (P-D-274).
const STORED_ROW_DOES_NOT_READ: &str = "a stored quota row does not read";

/// types-registry did not answer: 503 `QUOTA_REGISTRY_UNAVAILABLE`, nothing stored. The
/// registry's own text is logged here and never answered: the wire carries a fixed sentence.
pub(crate) fn registry_unavailable(detail: &str) -> CanonicalError {
    tracing::warn!(
        detail,
        "bss-products: types-registry did not answer a quota read"
    );
    DomainError::QuotaUnavailable {
        code: QUOTA_REGISTRY_UNAVAILABLE,
        detail: "types-registry did not answer".to_owned(),
    }
    .into()
}
