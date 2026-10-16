// Updated: 2026-10-06 by Constructor Tech
//! Consumer contract for tenant-scoped credential operations (ADR-0004,
//! "Read actions follow the projection"; ADR-0007 for writes).
//!
//! Defines anti-enumerating reads over one item shape — the record
//! ([`crate::models::Credential`]) and its value ([`crate::models::Secret`]),
//! served by a separate method — and explicit optimistic-concurrency
//! preconditions on every write.

use crate::models::{
    Credential, CredentialListItem, CredentialPatch, CredentialWrite, PutOutcome, PutPrecondition,
    Secret, SecretRef, Validator, WritePrecondition,
};
use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;

/// Consumer-facing API trait for credential storage operations.
///
/// Every fallible method returns [`CanonicalError`]; for typed dispatch,
/// project it with [`CredStoreError`](crate::CredStoreError)
/// (`.map_err(CredStoreError::from)`). Reason and field codes are in
/// [`crate::reason`] and [`crate::field`]. There is no `create`: `put` under
/// [`PutPrecondition::CreateOnly`] is create.
#[async_trait]
pub trait CredStoreClientV2: Send + Sync {
    /// Returns the credential record resolved for `key` through the tenant
    /// hierarchy, without its value (see [`Self::get_secret`]). `Ok(None)`
    /// means nothing the caller may see, including a record the PDP
    /// constraints exclude. Requires `read`.
    ///
    /// # Errors
    ///
    /// Projected by [`crate::CredStoreError`]:
    /// * `PermissionDenied` — `read` is denied, whether or not the record exists.
    /// * `ServiceUnavailable`, `Internal` — a dependency is unreachable or failed.
    async fn get_record(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<Credential>, CanonicalError>;

    /// Returns the secret value resolved for `key`. A winning record without
    /// a value is `Ok(None)`, the same as a missing one; an expired winner is
    /// an error, never a fall-through to an ancestor. Requires `read_secret`.
    ///
    /// # Errors
    ///
    /// Projected by [`crate::CredStoreError`]:
    /// * `PermissionDenied` — `read_secret` is denied, whether or not the record exists.
    /// * `FailedPrecondition` (`SECRET_EXPIRED`) — the winning record is past `expires_at`.
    /// * `ServiceUnavailable`, `Internal` — a dependency is unreachable or failed.
    async fn get_secret(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<Secret>, CanonicalError>;

    /// Creates or replaces the whole credential, record and value together.
    /// [`PutPrecondition::CreateOnly`] only creates; [`PutPrecondition::Exists`]
    /// and [`PutPrecondition::Matches`] only replace. A replace overwrites
    /// every field, so `expires_at: None` clears the expiry; use
    /// [`Self::patch`] to change some fields only. Requires `write`, plus
    /// `write_secret` when the value is set or removed.
    ///
    /// # Errors
    ///
    /// Projected by [`crate::CredStoreError`]:
    /// * `AlreadyExists` — `CreateOnly` over an existing own record.
    /// * `Aborted` — the precondition does not hold, or the type conflicts.
    /// * `InvalidArgument` — the write violates a type trait or lacks a required field.
    /// * `PermissionDenied` — a required action is denied.
    /// * `ServiceUnavailable`, `Internal` — a dependency is unreachable or failed.
    async fn put(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        write: CredentialWrite,
        precondition: PutPrecondition,
    ) -> Result<PutOutcome, CanonicalError>;

    /// Applies a partial change with JSON Merge Patch semantics: a present
    /// field is applied as [`Self::put`] would apply it, an absent one is
    /// kept. Never creates. Requires `write` for metadata fields and
    /// `write_secret` for `secret`.
    ///
    /// # Errors
    ///
    /// Projected by [`crate::CredStoreError`]:
    /// * `NotFound` — the caller holds no own record under `key`.
    /// * `Aborted` — the `Matches` validator no longer holds, or the patch changes the type.
    /// * `InvalidArgument` — an empty patch or a violated type trait.
    /// * `FailedPrecondition` (`UNSUPPORTED_TRANSITION`) — an unsupported `sharing` change.
    /// * `PermissionDenied` — a required action is denied.
    /// * `ServiceUnavailable`, `Internal` — a dependency is unreachable or failed.
    async fn patch(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        patch: CredentialPatch,
        precondition: WritePrecondition,
    ) -> Result<Validator, CanonicalError>;

    /// Deletes the caller's own record and its value, and frees the
    /// reference at once. Requires `delete`.
    ///
    /// # Errors
    ///
    /// Projected by [`crate::CredStoreError`]:
    /// * `NotFound` — the caller holds no own record under `key`.
    /// * `Aborted` — the `Matches` validator no longer holds.
    /// * `PermissionDenied` — `delete` is denied.
    /// * `ServiceUnavailable`, `Internal` — a dependency is unreachable or failed.
    async fn delete(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        precondition: WritePrecondition,
    ) -> Result<(), CanonicalError>;

    /// Lists the credentials visible to the caller's tenant, walking up its
    /// ancestor chain only: one item per reference, reduced as
    /// [`Self::get_record`] would. Selecting `secret` adds each readable
    /// item's value to the same page. Requires `list`, or `read_secret` when
    /// `secret` is selected.
    ///
    /// # Errors
    ///
    /// Projected by [`crate::CredStoreError`]:
    /// * `PermissionDenied` — a required action is denied, even when no record exists.
    /// * `InvalidArgument` — an unsupported filter, order, selection, limit or cursor.
    /// * `ServiceUnavailable`, `Internal` — a dependency is unreachable or failed.
    async fn list(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<CredentialListItem>, CanonicalError>;
}
