//! Local (in-process) client for the `AuthZ` resolver.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, EvaluationRequest, EvaluationResponse};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::PlatformSecurityContext;

use super::{DomainError, Service};

/// Local client wrapping the service.
#[domain_model]
pub struct AuthZResolverLocalClient {
    svc: Arc<Service>,
}

impl AuthZResolverLocalClient {
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

/// Map an infrastructure `DomainError` onto the contract's `CanonicalError`.
/// Access denial is never surfaced here — it rides in `EvaluationResponse`.
///
/// Transient conditions (a plugin or the types registry being momentarily
/// unavailable) map to `CanonicalError::service_unavailable()` (HTTP 503) so
/// callers can retry, while genuine bugs (malformed plugin content, internal
/// invariants) map to `CanonicalError::internal()` (HTTP 500). Collapsing both
/// into `internal` would hide a retryable outage behind a non-retryable 500.
fn log_and_convert(op: &str, e: &DomainError) -> CanonicalError {
    tracing::error!(operation = op, error = ?e, "authz_resolver call failed");
    match e {
        DomainError::TypesRegistryUnavailable(_)
        | DomainError::PluginNotFound { .. }
        | DomainError::PluginUnavailable { .. } => CanonicalError::service_unavailable()
            .with_detail(e.to_string())
            .create(),
        DomainError::InvalidPluginInstance { .. } | DomainError::Internal(_) => {
            CanonicalError::internal(e.to_string()).create()
        }
    }
}

#[async_trait]
impl AuthZResolverApi for AuthZResolverLocalClient {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        // `evaluate` is a platform-plane method (`cpt-cf-adr-two-plane-auth`):
        // the transport authenticates the *calling workload* (service identity),
        // not an end user. `_ctx` is a plane marker only and carries no tenant
        // identity, so the PDP trusts the authorization `subject` supplied in
        // `req.subject` from the (service-authenticated) PEP — the trust model
        // per `DESIGN.md` (subject flows AuthN → PEP → PDP).
        self.svc
            .evaluate(req)
            .await
            .map_err(|e| log_and_convert("evaluate", &e))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashMap;

    use authz_resolver_sdk::models::{Action, EvaluationRequestContext, Resource, Subject};
    use toolkit::client_hub::ClientHub;

    use super::*;

    fn empty_hub_client() -> AuthZResolverLocalClient {
        let svc = Arc::new(Service::new(
            Arc::new(ClientHub::default()),
            "constructorfabric".to_owned(),
        ));
        AuthZResolverLocalClient::new(svc)
    }

    fn sample_request() -> EvaluationRequest {
        EvaluationRequest {
            subject: Subject {
                id: uuid::Uuid::nil(),
                subject_type: None,
                properties: HashMap::new(),
            },
            action: Action {
                name: "list".to_owned(),
            },
            resource: Resource {
                resource_type: "gts.cf.core.users.user.v1~".to_owned(),
                id: None,
                properties: HashMap::new(),
            },
            context: EvaluationRequestContext {
                tenant_context: None,
                token_scopes: Vec::new(),
                require_constraints: false,
                capabilities: Vec::new(),
                supported_properties: Vec::new(),
                bearer_token: None,
            },
        }
    }

    /// With an empty `ClientHub` the service cannot resolve types-registry (a
    /// transient condition), so `evaluate` surfaces
    /// `DomainError::TypesRegistryUnavailable`, which `log_and_convert` maps
    /// onto the retryable `CanonicalError::ServiceUnavailable` (HTTP 503)
    /// rather than a non-retryable 500. This exercises `new`, the
    /// `AuthZResolverApi::evaluate` impl, and `log_and_convert`.
    #[tokio::test]
    async fn evaluate_maps_transient_domain_error_to_service_unavailable() {
        let client = empty_hub_client();

        let err = client
            .evaluate(PlatformSecurityContext::outbound_marker(), sample_request())
            .await
            .expect_err("evaluation must fail without a resolvable plugin");

        assert!(
            matches!(err, CanonicalError::ServiceUnavailable { .. }),
            "transient domain errors must map to CanonicalError::ServiceUnavailable, got: {err:?}"
        );
    }

    /// Transient variants (registry/plugin momentarily unavailable) map to the
    /// retryable 503, while genuine bugs map to the non-retryable 500. This
    /// guards the semantic distinction that `log_and_convert` must preserve.
    #[test]
    fn log_and_convert_distinguishes_transient_from_internal() {
        let transient = [
            DomainError::TypesRegistryUnavailable("down".to_owned()),
            DomainError::PluginNotFound {
                vendor: "acme".to_owned(),
            },
            DomainError::PluginUnavailable {
                gts_id: "gts".to_owned(),
                reason: "not ready".to_owned(),
            },
        ];
        for e in &transient {
            assert!(
                matches!(
                    log_and_convert("evaluate", e),
                    CanonicalError::ServiceUnavailable { .. }
                ),
                "{e:?} must map to ServiceUnavailable"
            );
        }

        let internal = [
            DomainError::InvalidPluginInstance {
                gts_id: "gts".to_owned(),
                reason: "malformed".to_owned(),
            },
            DomainError::Internal("boom".to_owned()),
        ];
        for e in &internal {
            assert!(
                matches!(
                    log_and_convert("evaluate", e),
                    CanonicalError::Internal { .. }
                ),
                "{e:?} must map to Internal"
            );
        }
    }
}
