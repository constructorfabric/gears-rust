use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;

use crate::domain::error::DomainError;

/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-error-mapping:p1
///
/// Map scope errors to domain errors.
pub(crate) fn map_scope_error(e: ScopeError) -> DomainError {
    // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-scope
    match e {
        ScopeError::Denied(msg) => DomainError::forbidden(msg),
        ScopeError::Invalid(msg) => DomainError::internal(format!("scope invalid: {msg}")),
        ScopeError::Db(e) => DomainError::Database(DbError::Sea(e)),
        ScopeError::TenantNotInScope { tenant_id } => {
            DomainError::forbidden(format!("tenant {tenant_id} not in scope"))
        }
        // `ScopeError` is `#[non_exhaustive]`.
        other => DomainError::internal(format!("unhandled scope error: {other}")),
    }
    // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-scope
}
