//! `DbError` / `ScopeError` → `RepositoryError`, applied the same way by both
//! database repositories.
//!
//! Constraint violations are classified with the backend-neutral
//! `ScopeError::is_unique_violation` / `is_foreign_key_violation`, never by
//! message text. A scope rejection (`Denied`, `TenantNotInScope`) is a bug in
//! the repository, which builds every scope itself (a write from the row's own
//! tenant, a read from the caller's tenants), so it is `Internal` like every
//! other unexpected failure.

use std::fmt::Display;

use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;
use uuid::Uuid;

use crate::domain::model::{Route, Upstream};
use crate::domain::repo::RepositoryError;

impl From<DbError> for RepositoryError {
    fn from(err: DbError) -> Self {
        Self::Internal(format!("database error: {err}"))
    }
}

/// Any failure without a domain meaning.
pub(super) fn internal(err: ScopeError) -> RepositoryError {
    RepositoryError::Internal(err.to_string())
}

/// Insert of an upstream row: a unique violation is the tenant's alias or
/// the ID (child rows are canonical and never collide).
pub(super) fn upstream_create(err: ScopeError, upstream: &Upstream) -> RepositoryError {
    upstream_conflict(
        err,
        upstream,
        format!(
            "alias '{}' or id '{}' already exists",
            upstream.alias, upstream.id
        ),
    )
}

/// Update of an upstream row: the ID is the row's own, so a unique violation
/// can only be the tenant's alias.
pub(super) fn upstream_update(err: ScopeError, upstream: &Upstream) -> RepositoryError {
    upstream_conflict(
        err,
        upstream,
        format!("alias '{}' already exists for tenant", upstream.alias),
    )
}

fn upstream_conflict(err: ScopeError, upstream: &Upstream, detail: String) -> RepositoryError {
    if err.is_unique_violation() {
        return RepositoryError::Conflict {
            entity: "upstream",
            resource: upstream.alias.clone(),
            detail,
        };
    }
    internal(err)
}

/// Insert of a route row: a unique violation is the ID; a foreign key
/// violation means the upstream does not exist for the route's tenant.
pub(super) fn route_create(err: ScopeError, route: &Route) -> RepositoryError {
    if err.is_unique_violation() {
        return RepositoryError::Conflict {
            entity: "route",
            resource: route.id.to_string(),
            detail: "route id already exists".to_owned(),
        };
    }
    if err.is_foreign_key_violation() {
        return not_found("upstream", route.upstream_id);
    }
    internal(err)
}

pub(super) fn not_found(entity: &'static str, id: Uuid) -> RepositoryError {
    RepositoryError::NotFound { entity, id }
}

/// A stored row that cannot be decoded. Never written through the
/// repositories, so it is an internal fault, not a bad request.
pub(super) fn invalid_stored(entity: &str, id: Uuid, detail: impl Display) -> RepositoryError {
    RepositoryError::Internal(format!("stored {entity} {id} is invalid: {detail}"))
}
