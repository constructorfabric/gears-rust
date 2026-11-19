// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-28 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-rest-odata:p1
//! REST API route definitions using `OperationBuilder`.

use crate::api::rest::{dto, handlers};
use crate::gear::{ConcreteGroupService, ConcreteMembershipService};
use axum::Router;
use std::sync::Arc;
use toolkit::api::OpenApiRegistry;

mod groups;
mod memberships;

/// Register all routes for the resource-group gear.
#[allow(clippy::needless_pass_by_value)]
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    group_service: Arc<ConcreteGroupService>,
    membership_service: Arc<ConcreteMembershipService>,
) -> Router {
    router = groups::register_group_routes(router, openapi);
    router = memberships::register_membership_routes(router, openapi);

    router = router
        .layer(axum::Extension(group_service))
        .layer(axum::Extension(membership_service));

    router
}
