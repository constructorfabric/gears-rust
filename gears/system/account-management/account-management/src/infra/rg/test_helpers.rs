//! Test helpers shared across crate-internal integration tests for the
//! Resource Group SDK adapter. Promoted to `pub(crate)` (compiled
//! under `#[cfg(test)]` only) so service-level tests in
//! `domain/tenant/service/service_tests.rs` can wire the production
//! [`super::RgResourceOwnershipChecker`] against a slow / empty fake
//! without re-stubbing the full ~15-method `ResourceGroupClient`
//! trait at every call site. The in-file fakes inside
//! `super::checker::tests` exercise the checker in isolation; this
//! gear exists for the cross-gear integration use case.

#![cfg(test)]

use std::time::Duration;

use async_trait::async_trait;
use resource_group_sdk::{
    CreateGroupRequest, CreateTypeRequest, ResourceGroup, ResourceGroupClient,
    ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth, UpdateGroupRequest,
    UpdateTypeRequest,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Minimal `ResourceGroupClient` fake whose `list_groups` sleeps for
/// `delay` before returning an empty page. Any other trait method is
/// `unreachable!()` — service-level integration tests exercise only
/// the soft-delete probe path. Use with
/// `#[tokio::test(start_paused = true)]` to verify the production
/// timeout boundary fires deterministically.
pub struct SlowRgClient {
    pub delay: Duration,
}

impl SlowRgClient {
    pub fn new(delay: Duration) -> Self {
        Self { delay }
    }
}

#[async_trait]
impl ResourceGroupClient for SlowRgClient {
    async fn create_type(
        &self,
        _ctx: &SecurityContext,
        _request: CreateTypeRequest,
    ) -> Result<ResourceGroupType, CanonicalError> {
        unreachable!()
    }
    async fn get_type(
        &self,
        _ctx: &SecurityContext,
        _code: &str,
    ) -> Result<ResourceGroupType, CanonicalError> {
        unreachable!()
    }
    async fn list_types(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupType>, CanonicalError> {
        unreachable!()
    }
    async fn update_type(
        &self,
        _ctx: &SecurityContext,
        _code: &str,
        _request: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, CanonicalError> {
        unreachable!()
    }
    async fn delete_type(&self, _ctx: &SecurityContext, _code: &str) -> Result<(), CanonicalError> {
        unreachable!()
    }
    async fn create_group(
        &self,
        _ctx: &SecurityContext,
        _request: CreateGroupRequest,
    ) -> Result<ResourceGroup, CanonicalError> {
        unreachable!()
    }
    async fn get_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<ResourceGroup, CanonicalError> {
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
    async fn update_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _request: UpdateGroupRequest,
    ) -> Result<ResourceGroup, CanonicalError> {
        unreachable!()
    }
    async fn delete_group(&self, _ctx: &SecurityContext, _id: Uuid) -> Result<(), CanonicalError> {
        unreachable!()
    }
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
    async fn add_membership(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _resource_type: &str,
        _resource_id: &str,
    ) -> Result<ResourceGroupMembership, CanonicalError> {
        unreachable!()
    }
    async fn remove_membership(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _resource_type: &str,
        _resource_id: &str,
    ) -> Result<(), CanonicalError> {
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
