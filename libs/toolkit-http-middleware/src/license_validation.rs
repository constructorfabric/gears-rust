//! License (feature-entitlement) validation middleware.
//!
//! The [`LicenseRequirementMap`] (`(method, path)` → required feature names) is
//! built by the consuming gear from its operation specs; this crate owns the
//! runtime type and the request-time middleware. Rejections are rendered under a
//! caller-supplied GTS `scope`.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use http::Method;
use toolkit_canonical_errors::CanonicalError;

use crate::common;

/// The always-available base feature; a route requiring only this needs no
/// additional entitlement.
pub const BASE_FEATURE: &str = "gts.cf.core.lic.feat.v1~cf.core.global.base.v1";

type LicenseKey = (Method, String);

/// Per-route required-feature lookup, plus the GTS `scope` under which
/// rejections are rendered.
#[derive(Clone, Default)]
pub struct LicenseRequirementMap {
    requirements: Arc<DashMap<LicenseKey, Vec<String>>>,
    scope: &'static str,
}

impl LicenseRequirementMap {
    /// Build the map from `(method, path)` → required feature-name pairs, rendering
    /// rejections under `scope`.
    #[must_use]
    pub fn from_pairs(
        scope: &'static str,
        pairs: impl IntoIterator<Item = (LicenseKey, Vec<String>)>,
    ) -> Self {
        let requirements = DashMap::new();
        for (key, features) in pairs {
            requirements.insert(key, features);
        }
        Self {
            requirements: Arc::new(requirements),
            scope,
        }
    }

    fn get(&self, method: &Method, path: &str) -> Option<Vec<String>> {
        self.requirements
            .get(&(method.clone(), path.to_owned()))
            .map(|v| v.value().clone())
    }
}

/// License validation middleware. Rejects with a canonical `permission_denied`
/// Problem (`reason` = `LICENSE_FEATURE_REQUIRED`) under `scope` when the route
/// requires a non-base feature.
pub async fn license_validation_middleware(
    State(map): State<LicenseRequirementMap>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let path = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(|| req.uri().path().to_owned(), |p| p.as_str().to_owned());

    let path = common::resolve_path(&req, path.as_str());

    let Some(required) = map.get(&method, &path) else {
        return next.run(req).await;
    };

    // TODO: this is a stub implementation
    // We need first to implement plugin and get its client from client_hub
    // Plugin should provide an interface to get a list of global features (features that are not scoped to particular resource)
    if required.iter().any(|r| r != BASE_FEATURE) {
        // `instance` / `trace_id` are filled by the canonical error
        // middleware (`toolkit::api::canonical_error_middleware`) on the way
        // out — this middleware sits inside its layer.
        return CanonicalError::scoped_permission_denied(map.scope)
            .with_reason("LICENSE_FEATURE_REQUIRED")
            .create()
            .into_response();
    }

    next.run(req).await
}
