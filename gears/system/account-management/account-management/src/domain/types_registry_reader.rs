//! Registry caller selection (P33).
//!
//! Request reads use [`TypesRegistryApi`] with the unchanged [`SecurityContext`].
//! Bootstrap, retention and the reaper use [`PlatformTypesRegistryApi`] without
//! inventing a caller context. Shared helpers take [`TypesRegistryReader`].
//! A failed tenant read never retries through the platform API.

use std::collections::HashMap;

use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::{
    GtsTypeId, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, ReadOptions, TypeSchema,
    TypesRegistryApi, TypesRegistryApiExt,
};
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The registry and the caller a read is made for.
#[domain_model]
#[derive(Clone, Copy)]
pub enum TypesRegistryReader<'a> {
    /// A read for the request whose context `ctx` is.
    Caller {
        api: &'a dyn TypesRegistryApi,
        ctx: &'a SecurityContext,
    },
    /// A platform operation's own read: there is no request and no caller.
    Platform(&'a dyn PlatformTypesRegistryApi),
}

impl<'a> TypesRegistryReader<'a> {
    /// A read for `ctx`'s request.
    #[must_use]
    pub fn caller(api: &'a dyn TypesRegistryApi, ctx: &'a SecurityContext) -> Self {
        Self::Caller { api, ctx }
    }

    /// `err`, a failed read of this reader, as a domain error: a refusal of
    /// the request's caller is the caller's, of the platform's own
    /// credential internal (see [`DomainError::caller_registry_failure`]).
    #[must_use]
    pub fn failure(self, err: CanonicalError, context: impl std::fmt::Display) -> DomainError {
        match self {
            Self::Caller { .. } => DomainError::caller_registry_failure(err, context),
            Self::Platform(_) => DomainError::platform_registry_failure(err, context),
        }
    }

    /// See [`TypesRegistryApiExt::get_type_schema`].
    ///
    /// # Errors
    /// The registry's.
    pub async fn get_type_schema(
        self,
        type_id: &GtsTypeId,
        options: ReadOptions,
    ) -> Result<TypeSchema, CanonicalError> {
        match self {
            Self::Caller { api, ctx } => api.get_type_schema(ctx, type_id, options).await,
            Self::Platform(api) => {
                api.get_type_schema(
                    &PlatformSecurityContext::outbound_marker(),
                    type_id,
                    options,
                )
                .await
            }
        }
    }

    /// See [`TypesRegistryApiExt::resolve_type_id`].
    ///
    /// # Errors
    /// The registry's.
    pub async fn resolve_type_id(self, type_uuid: Uuid) -> Result<GtsTypeId, CanonicalError> {
        match self {
            Self::Caller { api, ctx } => api.resolve_type_id(ctx, type_uuid).await,
            Self::Platform(api) => {
                api.resolve_type_id(&PlatformSecurityContext::outbound_marker(), type_uuid)
                    .await
            }
        }
    }

    /// See [`TypesRegistryApiExt::resolve_type_ids`].
    ///
    /// # Errors
    /// The registry's.
    pub async fn resolve_type_ids(
        self,
        type_uuids: &[Uuid],
    ) -> Result<HashMap<Uuid, Option<GtsTypeId>>, CanonicalError> {
        match self {
            Self::Caller { api, ctx } => api.resolve_type_ids(ctx, type_uuids).await,
            Self::Platform(api) => {
                api.resolve_type_ids(&PlatformSecurityContext::outbound_marker(), type_uuids)
                    .await
            }
        }
    }
}
