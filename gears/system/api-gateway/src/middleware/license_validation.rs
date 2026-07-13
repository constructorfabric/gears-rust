use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use http::Method;
use std::sync::Arc;
use toolkit::api::OperationSpec;
use toolkit_gts::gts_id;

use crate::middleware::common;
use crate::middleware::errors::ApiGatewayRouteError;

const BASE_FEATURE: &str = gts_id!("cf.core.lic.feat.v1~cf.core.global.base.v1");

type LicenseKey = (Method, String);

#[derive(Clone)]
pub struct LicenseRequirementMap {
    requirements: Arc<DashMap<LicenseKey, Vec<String>>>,
}

impl LicenseRequirementMap {
    #[must_use]
    pub fn from_specs(specs: &[OperationSpec]) -> Self {
        let requirements = DashMap::new();

        for spec in specs {
            if let Some(req) = spec.license_requirement.as_ref() {
                requirements.insert(
                    (spec.method.clone(), spec.path.clone()),
                    req.license_names.clone(),
                );
            }
        }

        Self {
            requirements: Arc::new(requirements),
        }
    }

    fn get(&self, method: &Method, path: &str) -> Option<Vec<String>> {
        self.requirements
            .get(&(method.clone(), path.to_owned()))
            .map(|v| v.value().clone())
    }
}

pub async fn license_validation_middleware(
    map: LicenseRequirementMap,
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
        return ApiGatewayRouteError::permission_denied()
            .with_reason("LICENSE_FEATURE_REQUIRED")
            .create()
            .into_response();
    }

    next.run(req).await
}
