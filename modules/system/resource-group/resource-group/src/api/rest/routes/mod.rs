// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-rest-odata:p1
//! REST API route definitions using `OperationBuilder`.

use crate::api::rest::{dto, handlers};
use crate::module::{ConcreteGroupService, ConcreteMembershipService, ConcreteTypeService};
use axum::Router;
use modkit::api::OpenApiRegistry;
use std::sync::Arc;

mod groups;
mod memberships;
mod types;

/// Register all routes for the resource-group module.
#[allow(clippy::needless_pass_by_value)]
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    type_service: Arc<ConcreteTypeService>,
    group_service: Arc<ConcreteGroupService>,
    membership_service: Arc<ConcreteMembershipService>,
) -> Router {
    router = types::register_type_routes(router, openapi);
    router = groups::register_group_routes(router, openapi);
    router = memberships::register_membership_routes(router, openapi);

    router = router
        .layer(axum::Extension(type_service))
        .layer(axum::Extension(group_service))
        .layer(axum::Extension(membership_service));

    router
}
