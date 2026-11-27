//! Test helpers shared across crate-internal integration tests for the
//! Resource Group SDK adapter. Promoted to `pub(crate)` (compiled under
//! `#[cfg(test)]` only) so service-level tests in
//! `domain/tenant/service/service_tests.rs` can wire the production
//! [`super::RgResourceOwnershipChecker`] against a slow fake. The in-file
//! fakes inside `super::checker::tests` exercise the checker in isolation.

#![cfg(test)]

use std::time::Duration;

use async_trait::async_trait;
use resource_group_sdk::{
    ResourceGroup, ResourceGroupMembership, ResourceGroupReadHierarchy, ResourceGroupWithDepth,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Minimal `ResourceGroupReadHierarchy` fake whose `list_groups` sleeps
/// for `delay` before returning an empty page. Any other trait method is
/// `unreachable!()` — service-level integration tests exercise only the
/// soft-delete probe path. Use with `#[tokio::test(start_paused = true)]`
/// to verify the production timeout boundary fires deterministically.
pub struct SlowRgClient {
    pub delay: Duration,
}

impl SlowRgClient {
    pub fn new(delay: Duration) -> Self {
        Self { delay }
    }
}

#[async_trait]
impl ResourceGroupReadHierarchy for SlowRgClient {
    async fn get_group_descendants(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        unreachable!()
    }
    async fn get_group_ancestors(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        unreachable!()
    }
    async fn list_groups(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, CanonicalError> {
        tokio::time::sleep(self.delay).await;
        Ok(Page::empty(1))
    }
    async fn get_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<ResourceGroup, CanonicalError> {
        unreachable!()
    }
    async fn list_memberships(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, CanonicalError> {
        unreachable!()
    }
}
