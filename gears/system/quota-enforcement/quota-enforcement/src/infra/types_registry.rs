//! `TypesRegistryContracts`: the `ContractRegistry` port over the platform
//! `types-registry` SDK (ADR-0007).
//!
//! Resolution reads the registry's materialized artifacts of a selected type:
//! its authored `content` (the leaf's own `x-gts-abstract`), its
//! `resolved_schema` (every parent and `gts://` reference inlined) and its
//! `effective_traits` (chain-merged). The payload validator the catalogue uses
//! compiles from them with the SDK (`TypeSchema::compile_payload_validator`),
//! so the declared dialect and the GTS value constraints are part of the
//! contract. The parent chain comes from the type id itself.
//!
//! Discovery (`derived_types`) reads the listing alone (content and traits,
//! every page) and resolves nothing, so a contract nobody configured cannot
//! fail bootstrap. A request's read goes through `TypesRegistryApi` under the
//! caller's context; the gear's own reads through `PlatformTypesRegistryApi`.
//! Both clients are consumed contracts, resolved from the hub per call (they
//! are wired after `init`). A deleted entity is absent. Every call is bounded
//! by a deadline, and every failure lifts to a domain error through a named
//! function (DE1302).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gts::{GtsId, GtsInstanceId, GtsTypeId};
use serde_json::Value;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsIdPattern, LifecycleStatus, ListEntitiesRequest,
    PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, Projection, ReadOptions, TypeSchema,
    TypesRegistryApi, TypesRegistryApiExt,
};

use crate::domain::error::{CallerRefusal, DomainError};
use crate::domain::ports::contracts::{
    ContractRegistry, DiscoveredType, RegisteredType, RegistryCaller,
};

const LOG_TARGET: &str = "qe.bootstrap";

/// Default budget for one registry call.
pub const DEFAULT_REGISTRY_DEADLINE: Duration = Duration::from_secs(10);

/// The adapter over the registry clients the hub holds.
pub struct TypesRegistryContracts {
    hub: Arc<ClientHub>,
    deadline: Duration,
}

/// `options` selecting exactly `fields`.
fn select(fields: &[EntityField]) -> ReadOptions {
    ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(fields)))
}

/// The parent chain of `id`, nearest parent first; empty for a root type.
fn ancestors(id: &GtsTypeId) -> Vec<GtsTypeId> {
    GtsId::try_new(id.as_ref())
        .map(|parsed| {
            let mut chain = parsed.chain_ids();
            chain.pop();
            chain.iter().rev().map(|id| GtsTypeId::new(id)).collect()
        })
        .unwrap_or_default()
}

/// `x-gts-abstract: true` on the authored document itself.
fn is_abstract(content: Option<&Value>) -> bool {
    content.and_then(|c| c.get("x-gts-abstract")) == Some(&Value::Bool(true))
}

impl TypesRegistryContracts {
    /// Bind to the hub with the default per-call deadline.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self {
            hub,
            deadline: DEFAULT_REGISTRY_DEADLINE,
        }
    }

    /// Override the budget one registry call may take.
    #[must_use]
    pub const fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    fn platform(&self) -> Result<Arc<dyn PlatformTypesRegistryApi>, DomainError> {
        self.hub
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| DomainError::TypesRegistryUnavailable(e.to_string()))
    }

    fn tenant(&self) -> Result<Arc<dyn TypesRegistryApi>, DomainError> {
        self.hub
            .get::<dyn TypesRegistryApi>()
            .map_err(|e| DomainError::TypesRegistryUnavailable(e.to_string()))
    }

    /// Runs one registry call under the deadline; every failure lifts.
    async fn bounded<T>(
        &self,
        what: &'static str,
        call: impl Future<Output = Result<T, CanonicalError>>,
    ) -> Result<T, DomainError> {
        match tokio::time::timeout(self.deadline, call).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(unavailable(what, &err)),
            Err(_elapsed) => Err(timed_out(what, self.deadline)),
        }
    }

    /// One exact entity read under the deadline. `Ok(None)` for `NotFound`;
    /// every other failure lifts with `lift`.
    async fn bounded_entity<T>(
        &self,
        what: &'static str,
        lift: fn(&str, &CanonicalError) -> DomainError,
        call: impl Future<Output = Result<T, CanonicalError>>,
    ) -> Result<Option<T>, DomainError> {
        match tokio::time::timeout(self.deadline, call).await {
            Ok(Ok(value)) => Ok(Some(value)),
            Ok(Err(CanonicalError::NotFound { .. })) => Ok(None),
            Ok(Err(err)) => Err(lift(what, &err)),
            Err(_elapsed) => Err(timed_out(what, self.deadline)),
        }
    }

    /// One active type with `fields` selected. `Ok(None)` when it is not
    /// registered or deleted.
    async fn fetch_type(
        &self,
        caller: RegistryCaller<'_>,
        id: &GtsTypeId,
        fields: &[EntityField],
    ) -> Result<Option<TypeSchema>, DomainError> {
        let options = select(fields);
        let fetched = match caller {
            RegistryCaller::Platform => {
                let platform = self.platform()?;
                let ctx = PlatformSecurityContext::outbound_marker();
                self.bounded_entity(
                    "get_type_schema",
                    unavailable,
                    platform.get_type_schema(&ctx, id, options),
                )
                .await?
            }
            RegistryCaller::Request(ctx) => {
                let tenant = self.tenant()?;
                self.bounded_entity(
                    "get_type_schema",
                    request_failure,
                    tenant.get_type_schema(ctx, id, options),
                )
                .await?
            }
        };
        Ok(fetched.filter(|t| t.lifecycle_status == LifecycleStatus::Active))
    }
}

/// The resolved type the catalogue consumes, from the registry's
/// materialized artifacts.
fn resolve(snapshot: TypeSchema) -> Result<RegisteredType, DomainError> {
    // Compiled from the borrowed snapshot; the documents then move out.
    let payload = snapshot.compile_payload_validator();
    let is_abstract = is_abstract(snapshot.content.as_ref());
    let ancestors = ancestors(&snapshot.type_id);
    let TypeSchema {
        type_id: id,
        content,
        resolved_schema,
        effective_traits,
        ..
    } = snapshot;
    let schema =
        resolved_schema.ok_or_else(|| drift(&id, "the registry returned no resolved schema"))?;
    if content.is_none() {
        return Err(drift(&id, "the registry returned no content"));
    }
    let effective_traits =
        effective_traits.ok_or_else(|| drift(&id, "the registry returned no effective traits"))?;
    Ok(RegisteredType {
        id,
        is_abstract,
        ancestors,
        effective_traits,
        schema,
        payload,
    })
}

#[async_trait]
impl ContractRegistry for TypesRegistryContracts {
    async fn type_schema(
        &self,
        caller: RegistryCaller<'_>,
        id: &GtsTypeId,
    ) -> Result<Option<RegisteredType>, DomainError> {
        let fields = [
            EntityField::Content,
            EntityField::ResolvedSchema,
            EntityField::EffectiveTraits,
        ];
        match self.fetch_type(caller, id, &fields).await? {
            Some(snapshot) => resolve(snapshot).map(Some),
            None => Ok(None),
        }
    }

    async fn derived_types(&self, base: &GtsTypeId) -> Result<Vec<DiscoveredType>, DomainError> {
        let pattern = GtsIdPattern::try_new(&format!("{base}*"))
            .map_err(|e| DomainError::Internal(format!("listing pattern for {base}: {e}")))?;
        let query = ListEntitiesRequest::matching(pattern).with_projection(Projection::Select(
            FieldSelection::with(&[EntityField::Content, EntityField::EffectiveTraits]),
        ));
        let platform = self.platform()?;
        let ctx = PlatformSecurityContext::outbound_marker();
        let listed = self
            .bounded("list_type_schemas", platform.list_type_schemas(&ctx, query))
            .await?;
        // The ancestry check keeps the base itself and look-alike ids out.
        listed
            .into_iter()
            .filter(|s| s.type_id != *base && ancestors(&s.type_id).contains(base))
            .map(|s| {
                let content = s
                    .content
                    .as_ref()
                    .ok_or_else(|| drift(&s.type_id, "the registry returned no content"))?;
                let is_abstract = is_abstract(Some(content));
                let declared_traits = s.effective_traits.ok_or_else(|| {
                    drift(&s.type_id, "the registry returned no effective traits")
                })?;
                Ok(DiscoveredType {
                    id: s.type_id,
                    is_abstract,
                    declared_traits,
                })
            })
            .collect()
    }

    async fn instance_type(&self, id: &GtsInstanceId) -> Result<Option<GtsTypeId>, DomainError> {
        let platform = self.platform()?;
        let ctx = PlatformSecurityContext::outbound_marker();
        let instance = self
            .bounded_entity(
                "get_instance",
                unavailable,
                platform.get_instance(&ctx, id, select(&[])),
            )
            .await?;
        Ok(instance
            .filter(|i| i.lifecycle_status == LifecycleStatus::Active)
            .map(|i| i.type_id))
    }
}

/// The caller's refusal a request's read carries, if `err` is one: the
/// registry refused the caller's credential (401) or access (403).
pub(crate) fn refused_caller(err: &CanonicalError) -> Option<DomainError> {
    match err {
        CanonicalError::Unauthenticated { .. } => Some(DomainError::RegistryRefusedCaller(
            CallerRefusal::Unauthenticated,
        )),
        CanonicalError::PermissionDenied { .. } => Some(DomainError::RegistryRefusedCaller(
            CallerRefusal::PermissionDenied,
        )),
        _ => None,
    }
}

/// A request's failed read: the caller's refusal, else an outage.
fn request_failure(what: &str, err: &CanonicalError) -> DomainError {
    refused_caller(err).unwrap_or_else(|| unavailable(what, err))
}

/// A transport or registry failure. The cause is kept as text: the domain
/// error is `Clone + Eq` and crosses the layer boundary as a value.
fn unavailable(what: &str, err: &CanonicalError) -> DomainError {
    DomainError::TypesRegistryUnavailable(format!("types registry `{what}` failed: {err}"))
}

/// The error for a registry call that missed its deadline.
fn timed_out(what: &str, deadline: Duration) -> DomainError {
    DomainError::TypesRegistryUnavailable(format!(
        "types registry did not answer `{what}` within {deadline:?}"
    ))
}

/// Registry-served artifacts this process cannot use: the registry and this
/// process disagree about a contract. Fail closed and say why.
fn drift(type_id: &GtsTypeId, detail: &str) -> DomainError {
    tracing::error!(
        target: LOG_TARGET,
        type_id = %type_id,
        detail,
        "a registered contract did not resolve"
    );
    DomainError::TypesRegistryUnavailable(format!("contract {type_id} did not resolve: {detail}"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "types_registry_tests.rs"]
mod types_registry_tests;
