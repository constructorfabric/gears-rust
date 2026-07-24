//! REST handlers for the credstore module.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use credstore_sdk::{ExpiryWrite, GtsId, SecretRef, SecretValue, SharingMode, WriteOptions};
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use super::dto::{CreateSecretRequestDto, GetSecretResponseDto, UpdateSecretRequestDto};
use crate::domain::error::DomainError;
use crate::domain::secret::model::{WritePrecondition, WriteSpec};
use crate::domain::secret::service::Service;

/// Concrete service alias for the handlers.
pub(crate) type ConcreteService = Service;

/// Parse an optional `If-Match` precondition (RFC 7232 §3.1). The GET handler
/// emits a strong, generation-bound `"<id>.<version>"` validator (`id` = the
/// row UUID, fresh per recreated secret — no ABA reuse across generations).
///
/// `If-Match` is `"*" / 1#entity-tag`: it may span multiple header lines and
/// each line may carry a comma-separated list, and the list matches if **any**
/// validator matches. We accept `*` (target must exist) or one-or-more strong
/// `"<id>.<version>"` validators; a single one yields [`WritePrecondition::Version`],
/// several yield [`WritePrecondition::AnyVersion`]. Weak validators (`W/"…"`)
/// and any other shape are a typed 400.
fn parse_if_match(
    headers: &axum::http::HeaderMap,
) -> Result<Option<WritePrecondition>, DomainError> {
    let mut lines = headers
        .get_all(axum::http::header::IF_MATCH)
        .iter()
        .peekable();
    if lines.peek().is_none() {
        return Ok(None);
    }
    let malformed = || DomainError::InvalidPrecondition {
        detail: "If-Match must be `*` or quoted `<id>.<version>` ETag(s)".to_owned(),
    };
    let mut validators: Vec<(uuid::Uuid, i64)> = Vec::new();
    for raw in lines {
        let line = raw.to_str().map_err(|_| DomainError::InvalidPrecondition {
            detail: "If-Match header is not valid ASCII".to_owned(),
        })?;
        for tag in line.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            // `*` matches any current representation → an existence check; it
            // subsumes any other member, so return immediately.
            if tag == "*" {
                return Ok(Some(WritePrecondition::Exists));
            }
            // Strong validator only: `"<id>.<version>"` (a UUID contains no
            // `.`, so the split is unambiguous).
            let parsed = tag
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .and_then(|inner| inner.split_once('.'))
                .and_then(|(id, version)| {
                    Some((
                        uuid::Uuid::parse_str(id).ok()?,
                        version.parse::<i64>().ok()?,
                    ))
                })
                .ok_or_else(malformed)?;
            validators.push(parsed);
        }
    }
    match validators.as_slice() {
        [] => Err(malformed()),
        [(id, version)] => Ok(Some(WritePrecondition::Version {
            id: *id,
            version: *version,
        })),
        _ => Ok(Some(WritePrecondition::AnyVersion(validators))),
    }
}

/// Parse the optional typed-write fields shared by the create/update DTOs
/// into [`WriteOptions`]: a full GTS type id (`GtsId`) and an RFC 3339
/// expiry. A `type` that is not a well-formed GTS type id and malformed
/// timestamps are typed 400s; whether a well-formed custom type actually
/// exists is decided by the service against the types-registry.
///
/// Expiry follows REST whole-value-replace semantics: a present `expires_at`
/// sets it ([`ExpiryWrite::Set`]) and an omitted one clears any stored expiry
/// ([`ExpiryWrite::Clear`]). (This differs from the SDK convenience
/// [`put`](credstore_sdk::CredStoreClientV1::put), which defaults to
/// [`ExpiryWrite::Preserve`](credstore_sdk::ExpiryWrite::Preserve).)
fn parse_write_options(
    secret_type: Option<&str>,
    expires_at: Option<&str>,
) -> Result<WriteOptions, DomainError> {
    let secret_type = secret_type
        .map(|input| {
            GtsId::try_new(input).map_err(|_| DomainError::TypeViolation {
                field: "type",
                reason: crate::domain::secret::typing::reasons::UNKNOWN_SECRET_TYPE,
                detail: format!("secret type must be a full GTS type id: {input}"),
            })
        })
        .transpose()?;
    let expires_at = expires_at
        .map(|raw| {
            time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
                .map(ExpiryWrite::Set)
                .map_err(|_| DomainError::TypeViolation {
                    field: "expires_at",
                    reason: "INVALID_EXPIRES_AT",
                    detail: "expires_at must be an RFC 3339 timestamp".to_owned(),
                })
        })
        .transpose()?
        .unwrap_or(ExpiryWrite::Clear);
    Ok(WriteOptions {
        secret_type,
        expires_at,
        // REST carries the optimistic-concurrency precondition in the `If-Match`
        // header (parsed into `WriteSpec::precondition`), not in `WriteOptions`.
        precondition: None,
    })
}

/// `POST /credstore/v1/secrets`
///
/// # Errors
///
/// Returns a canonical `Problem` envelope on invalid reference (400), access denied (403),
/// conflict (409), or service unavailable (503).
pub async fn create_secret(
    uri: axum::http::Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(body): Json<CreateSecretRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let key = SecretRef::new(body.reference).map_err(|e| {
        CanonicalError::from(DomainError::InvalidSecretRef {
            detail: e.to_string(),
        })
    })?;
    let opts = parse_write_options(body.secret_type.as_deref(), body.expires_at.as_deref())
        .map_err(CanonicalError::from)?;
    svc.put(
        &ctx,
        &key,
        SecretValue::from(body.value),
        WriteSpec::create(body.sharing.into()).with_opts(opts),
    )
    .await?;
    let location = format!("{}/{}", uri.path().trim_end_matches('/'), key.as_ref());
    Ok((
        StatusCode::CREATED,
        [(axum::http::header::LOCATION, location)],
    )
        .into_response())
}

/// `PUT /credstore/v1/secrets/{ref}`
///
/// Honours an optional `If-Match` precondition: a stale version yields a
/// canonical `Aborted` (409, `OPTIMISTIC_LOCK_FAILURE`).
///
/// # Errors
///
/// Returns a canonical `Problem` envelope on invalid reference / malformed
/// `If-Match` (400), access denied (403), unsupported transition (400),
/// version precondition failure (409), or service unavailable (503).
pub async fn put_secret(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(reference): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<UpdateSecretRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let precondition = parse_if_match(&headers)?;
    let key = SecretRef::new(reference).map_err(|e| {
        CanonicalError::from(DomainError::InvalidSecretRef {
            detail: e.to_string(),
        })
    })?;
    let opts = parse_write_options(body.secret_type.as_deref(), body.expires_at.as_deref())
        .map_err(CanonicalError::from)?;
    // An omitted `sharing` preserves the existing secret's mode on overwrite
    // (finding #8); `tenant` is only the create-via-upsert / class default.
    let (sharing, preserve_sharing) = match body.sharing {
        Some(s) => (s.into(), false),
        None => (SharingMode::Tenant, true),
    };
    svc.put(
        &ctx,
        &key,
        SecretValue::from(body.value),
        WriteSpec::upsert(sharing)
            .preserve_sharing(preserve_sharing)
            .with_precondition(precondition)
            .with_opts(opts),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `GET /credstore/v1/secrets/{ref}`
///
/// # Errors
///
/// Returns a canonical `Problem` envelope on invalid reference (400), access denied (403),
/// not found (404), or service unavailable (503).
pub async fn get_secret(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(reference): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let key = SecretRef::new(reference).map_err(|e| {
        CanonicalError::from(DomainError::InvalidSecretRef {
            detail: e.to_string(),
        })
    })?;
    match svc.get(&ctx, &key).await? {
        Some(ref resp) => {
            // Reject (don't lossily decode) a non-UTF-8 value before building headers.
            let dto = GetSecretResponseDto::try_from_response(resp)?;
            // Strong, generation-bound validator: the row UUID (fresh per
            // recreated secret) plus the per-generation monotonic counter, so
            // validators never repeat across delete+recreate (no ABA).
            let etag = format!("\"{}.{}\"", resp.id, resp.version);
            Ok((
                StatusCode::OK,
                [
                    (axum::http::header::ETAG, etag),
                    // Secret material must never be cached by intermediaries.
                    (axum::http::header::CACHE_CONTROL, "no-store".to_owned()),
                ],
                Json(dto),
            )
                .into_response())
        }
        None => Err(CanonicalError::from(DomainError::NotFound)),
    }
}

/// `DELETE /credstore/v1/secrets/{ref}`
///
/// Honours an optional `If-Match` precondition: a stale version yields a
/// canonical `Aborted` (409, `OPTIMISTIC_LOCK_FAILURE`).
///
/// # Errors
///
/// Returns a canonical `Problem` envelope on invalid reference / malformed
/// `If-Match` (400), access denied (403), not found (404), version precondition
/// failure (409), or service unavailable (503).
pub async fn delete_secret(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(reference): Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let precondition = parse_if_match(&headers)?;
    let key = SecretRef::new(reference).map_err(|e| {
        CanonicalError::from(DomainError::InvalidSecretRef {
            detail: e.to_string(),
        })
    })?;
    svc.delete(&ctx, &key, precondition).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
