//! Handler of the anonymous read surface.
//!
//! The one route of this gear a caller reaches without a token, so what it
//! may return is decided here by construction and not by a permission: only
//! the declarations flagged `anonymous_exposable`, resolved at the tenant the
//! caller names, as their effective values and nothing else.

use std::sync::Arc;

use axum::extract::RawQuery;
use axum::{Extension, Json};
use toolkit::api::canonical_prelude::*;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use crate::api::rest::public_dto::{PublicSettingDto, PublicSettingsDto};
use crate::domain::error::DomainError;
use crate::domain::resolution::ScopeTarget;
use crate::gear::ConcreteResolver;

/// The tenant a raw query string names: exactly one `tenant` parameter, a
/// valid UUID, and nothing else counts. Every pair whose key is `tenant` is
/// counted, a bare `tenant` with no `=` included, so `tenant&tenant=<uuid>` is
/// two and answers empty.
///
/// Parsed by hand because an extractor can reject a query before the handler
/// runs, and a rejection is a `400` that tells a prober which inputs are well
/// formed. Here every other shape (no `tenant`, an empty or malformed one, a
/// repeated one, bad percent-encoding, unknown extra parameters beside a good
/// one) is `None`, and the caller answers it with the one empty answer. A
/// UUID needs no percent-encoding, so a value carrying `%` is simply invalid.
fn tenant_of(raw: Option<&str>) -> Option<Uuid> {
    let mut named = raw?
        .split('&')
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
        .filter(|(key, _)| *key == "tenant")
        .map(|(_, value)| value);
    let only = named.next()?;
    if named.next().is_some() {
        return None;
    }
    Uuid::parse_str(only).ok()
}

/// Anonymous read of the exposable settings at one tenant.
///
/// `200` with the exposable settings and their effective values. Also `200`
/// with an empty list, and no other difference, for a tenant that is unknown,
/// absent from the query or not a UUID: the route never says which tenants
/// exist.
///
/// # Errors
/// `503` for a real failure, never an empty answer: the database, the tenant
/// resolver or the types registry being unreachable, so a caller can tell an
/// outage from nothing to show.
pub async fn read_public_settings(
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<DBProvider<DbError>>>,
    RawQuery(raw): RawQuery,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-1
    let Some(tenant) = tenant_of(raw.as_deref()) else {
        return Ok(Json(PublicSettingsDto::empty()));
    };
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-1
    Ok(Json(exposable_values(&resolver, &db, tenant).await?))
}

/// The exposable settings' effective values at `tenant`.
///
/// # Errors
/// [`DomainError::Unavailable`] for any failure other than the tenant being
/// unknown, which is the empty answer.
async fn exposable_values(
    resolver: &ConcreteResolver,
    db: &DBProvider<DbError>,
    tenant: Uuid,
) -> Result<PublicSettingsDto, DomainError> {
    // The tenant is looked up first, whatever the scope class of what is
    // flagged: an unknown tenant is the empty answer for a `local` setting as
    // much as for a `cascading` one, and an unreachable resolver is an
    // outage whether or not anything is flagged.
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-2
    match resolver.chain_of(tenant).await {
        Ok(_) => {}
        Err(DomainError::NotFound { .. }) => return Ok(PublicSettingsDto::empty()),
        Err(err) => return Err(outage(&err)),
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-2
    let conn = db
        .conn()
        .map_err(|err| DomainError::dependency_unavailable("database", "open a connection", err))?;
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-3
    let declarations = resolver
        .exposable_declarations(&conn)
        .await
        .map_err(|err| outage(&err))?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-3
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-4
    let outcomes = resolver
        .resolve_declarations(&conn, &declarations, ScopeTarget::Tenant(tenant))
        .await;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-4
    let mut items = Vec::with_capacity(declarations.len());
    for (declaration, outcome) in declarations.iter().zip(outcomes) {
        match outcome {
            Ok(effective) => {
                // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-5
                // Defensive: the flag and the classification are one rule in
                // the schema, so this cannot fail; if it ever did, the value
                // is withheld rather than published.
                if effective.data_classification != "public" || effective.secret_backed {
                    continue;
                }
                // @cpt-end:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-5
                items.push(PublicSettingDto {
                    key: declaration.key.clone(),
                    value: effective.value.clone(),
                });
            }
            // Retired since the selection was read: the setting is simply not
            // there to show. A tenant gone since the first lookup surfaces as
            // an internal failure and so as 503.
            Err(DomainError::Retired { .. }) => {}
            Err(err) => return Err(outage(&err)),
        }
    }
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-6
    Ok(PublicSettingsDto { items })
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-public-read:p2:inst-vr-public-6
}

/// A failure of the read as the caller sees it: unavailable, with nothing of
/// the dependency's own text on the wire; the cause is logged here.
fn outage(err: &DomainError) -> DomainError {
    match err {
        DomainError::Unavailable { .. } => DomainError::Unavailable {
            detail: "the public settings could not be read".to_owned(),
        },
        other => DomainError::dependency_unavailable(
            "settings service",
            "read the public settings",
            other,
        ),
    }
}
