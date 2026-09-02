// Created: 2026-08-24 by Constructor Tech
//! Deployment-time bootstrap adapter implementing the SDK
//! `ResourceGroupTypeBootstrap` trait.
//!
//! Thin pass-through to `TypeService`'s own unscoped methods — the same
//! methods RG's own `seed_types` uses at RG's init. See
//! `resource_group_sdk::ResourceGroupTypeBootstrap` for the full rationale
//! on why this surface bypasses `PolicyEnforcer` and why it must never be
//! exposed through REST.
//!
//! TEMPORARY: this bootstrap surface is a consequence of the GTS type
//! registry currently living inside this gear. If the registry is ever
//! split into its own gear, revisit this trait/impl — see the "Temporary"
//! section on `ResourceGroupTypeBootstrap`'s doc comment.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use resource_group_sdk::ResourceGroupTypeBootstrap;
use resource_group_sdk::models::{CreateTypeRequest, ResourceGroupType, UpdateTypeRequest};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::domain::repo::TypeRepositoryTrait;
use crate::domain::type_service::TypeService;

/// Adapter service exposing type-registry bootstrap via the SDK
/// `ResourceGroupTypeBootstrap` trait.
///
/// **Bypasses `AuthZ` enforcement** — delegates to `TypeService`'s unscoped
/// methods. This is by design: the only callers are other gears' `init`
/// paths, at a point in the toolkit lifecycle where `PolicyEnforcer` is
/// structurally unavailable (see the trait doc comment for the full
/// rationale). The in-process `ClientHub` path therefore skips `AuthZ`.
///
/// **Sealed after the bootstrap window closes.** `ClientHub` has no notion
/// of lifecycle phase, so removing this service's registration stops a
/// *new* lookup but does nothing about an `Arc` some caller already
/// cloned — and the doc comment's "never through REST, only from another
/// gear's `init`" promise was otherwise resting on that doc comment alone.
/// `seal` is called once, from `gear.rs`'s `register_rest` (which the
/// toolkit runtime only invokes after every gear's `init` and `post_init`
/// have completed, in both the in-process and out-of-process profiles), so
/// every method fails closed for a lookup and a retained handle alike from
/// that point on.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct RgTypeBootstrapService<TR: TypeRepositoryTrait> {
    type_service: Arc<TypeService<TR>>,
    sealed: AtomicBool,
}

impl<TR: TypeRepositoryTrait> RgTypeBootstrapService<TR> {
    /// Create a new `RgTypeBootstrapService`, open for calls until [`Self::seal`].
    #[must_use]
    pub fn new(type_service: Arc<TypeService<TR>>) -> Self {
        Self {
            type_service,
            sealed: AtomicBool::new(false),
        }
    }

    /// Close the bootstrap window. Idempotent, and irreversible for the
    /// lifetime of this instance -- there is no unseal, because there is no
    /// legitimate caller left to unseal it for.
    pub fn seal(&self) {
        self.sealed.store(true, Ordering::Release);
    }

    /// `Err` once sealed, naming neither the caller nor the method: this
    /// path is meant to be unreachable, not diagnosed.
    fn check_open(&self) -> Result<(), CanonicalError> {
        if self.sealed.load(Ordering::Acquire) {
            return Err(CanonicalError::internal(
                "ResourceGroupTypeBootstrap is only callable during gear init; \
                 the bootstrap window has closed",
            )
            .create());
        }
        Ok(())
    }
}

#[async_trait]
impl<TR: TypeRepositoryTrait> ResourceGroupTypeBootstrap for RgTypeBootstrapService<TR> {
    async fn get_type(
        &self,
        _ctx: &SecurityContext,
        code: &str,
    ) -> Result<ResourceGroupType, CanonicalError> {
        // Bypass AuthZ — see the trait-level doc comment on
        // `ResourceGroupTypeBootstrap`. `_ctx` carries no enforcement
        // weight here; it exists only so a future audit-correlation hook
        // has something to thread through.
        self.check_open()?;
        self.type_service
            .get_type_unscoped(code)
            .await
            .map_err(CanonicalError::from)
    }

    async fn create_type(
        &self,
        _ctx: &SecurityContext,
        request: CreateTypeRequest,
    ) -> Result<ResourceGroupType, CanonicalError> {
        // Bypass AuthZ — see the trait-level doc comment.
        self.check_open()?;
        self.type_service
            .create_type_unscoped(request)
            .await
            .map_err(CanonicalError::from)
    }

    async fn update_type(
        &self,
        _ctx: &SecurityContext,
        code: &str,
        request: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, CanonicalError> {
        // Bypass AuthZ — see the trait-level doc comment.
        self.check_open()?;
        self.type_service
            .update_type_unscoped(code, request)
            .await
            .map_err(CanonicalError::from)
    }
}
