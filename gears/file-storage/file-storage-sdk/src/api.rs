//! Inter-gear client trait for the file-storage control plane.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::FileStorageError;
use crate::models::{
    CreateFileOutcome, CustomMetadataPatch, EffectivePolicy, FileFetch, FileId, FileRecord,
    MultipartCompleteOutcome, MultipartIntent, MultipartPlan, MultipartStatus, NewFile,
    OwnerFilter, OwnerKind, Page, Policy, PolicyBody, PolicyScope, RetentionRule,
    RetentionRuleBody, RetentionScope, Storage, UploadTicket, VersionId, VersionRecord,
};

/// Public client trait other gears resolve from `ClientHub`.
///
/// Level 1 SDK: every control-plane operation runs in-process (no HTTP hop
/// to this gear's own REST surface); it returns models and signed URLs, and
/// never transfers file bytes itself — a caller still `PUT`s/`GET`s bytes
/// against the sidecar over the signed URLs this trait hands back. Streaming
/// upload/read *inside* the SDK (a seekable reader/writer that hides the
/// sidecar entirely) is a Level 2 feature and is not implemented by this
/// trait — see `docs/DESIGN.md`'s `sdk-facade` component for the split.
///
/// # Error envelope
///
/// Every fallible method returns `Result<_, FileStorageError>` — the same
/// canonical error envelope (`CanonicalError`) the control-plane REST API
/// maps `DomainError` into (`api::rest::error` in the impl crate); this
/// trait surfaces it unchanged.
///
/// # Conditional requests
///
/// `If-Match`/`If-None-Match`/`If-Match-Metadata` are explicit `Option<_>`
/// parameters, mirroring the control API's headers of the same name.
#[async_trait]
pub trait FileStorageClientV1: Send + Sync {
    // ── files ────────────────────────────────────────────────────────────────

    /// Create a file and presign its first content upload.
    ///
    /// With no `multipart` intent (or one whose computed plan collapses to a
    /// single part), returns [`CreateFileOutcome::SinglePart`] — the ordinary
    /// presigned-URL ticket. With a `multipart` intent whose plan has two or
    /// more parts, returns [`CreateFileOutcome::Multipart`] instead — no
    /// single-part version is pre-registered.
    ///
    /// `idempotency_key` is rejected together with a `multipart` intent (the
    /// stored idempotency record only fits a single-part ticket).
    /// `auto_bind` selects whether the upload itself binds the first content
    /// (single-part: the sidecar's finalize callback; multipart: `complete`)
    /// or the caller binds explicitly afterwards via [`Self::bind`].
    async fn create_file(
        &self,
        ctx: &SecurityContext,
        new: NewFile,
        idempotency_key: Option<String>,
        auto_bind: bool,
        multipart: Option<MultipartIntent>,
    ) -> Result<CreateFileOutcome, FileStorageError>;

    /// Get a file's metadata, honoring an optional `If-None-Match`.
    ///
    /// Returns [`FileFetch::NotModified`] when `if_none_match` matches the
    /// file's current content `ETag` (or is `"*"` and the file has any content
    /// `ETag`), mirroring the control API's conditional `304`.
    async fn get_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        if_none_match: Option<&str>,
    ) -> Result<FileFetch, FileStorageError>;

    /// List files (each with its custom metadata) for a mandatory owner
    /// filter, offset-paginated.
    async fn list_files(
        &self,
        ctx: &SecurityContext,
        owner: OwnerFilter,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Page<FileRecord>, FileStorageError>;

    /// `JSON`-merge-patch a file's custom metadata, optionally guarded by
    /// `If-Match-Metadata` (the file's `meta_version`).
    async fn update_metadata(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        patch: CustomMetadataPatch,
        if_match_metadata: Option<i64>,
    ) -> Result<FileRecord, FileStorageError>;

    /// Delete a file and all its versions. `if_match` (the content `ETag`, or
    /// `"*"`) is **required** — pass `Some("*")` to delete unconditionally
    /// when the `ETag` is unknown.
    async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        if_match: Option<&str>,
    ) -> Result<(), FileStorageError>;

    /// Issue a signed download URL pinned to the current content (or a
    /// specific `version_id`).
    async fn download_url(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        version_id: Option<VersionId>,
    ) -> Result<crate::models::DownloadTicket, FileStorageError>;

    // ── versions ─────────────────────────────────────────────────────────────

    /// List a file's content versions, newest first, offset-paginated.
    async fn list_versions(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Page<VersionRecord>, FileStorageError>;

    /// Presign a new content version on an existing file (bind it afterwards
    /// via [`Self::bind`]).
    async fn presign_version(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
    ) -> Result<UploadTicket, FileStorageError>;

    /// Swap the file's content pointer to `version_id` under optimistic CAS.
    /// `if_match` is the opaque content `ETag` (or `"*"`, or `None` for the
    /// first bind); rebinding already-bound content requires it.
    async fn bind(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        version_id: VersionId,
        if_match: Option<&str>,
    ) -> Result<FileRecord, FileStorageError>;

    /// Delete a single version. Deleting the file's only version is
    /// equivalent to deleting the file.
    async fn delete_version(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        version_id: VersionId,
    ) -> Result<(), FileStorageError>;

    // ── multipart upload ─────────────────────────────────────────────────────

    /// Initiate a standalone multipart upload session (the "new version of an
    /// existing file" path — the client binds manually via [`Self::bind`]).
    async fn initiate_multipart(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        declared_mime: &str,
        declared_size: u64,
        preferred_part_size: Option<u64>,
        concurrency: Option<u32>,
    ) -> Result<MultipartPlan, FileStorageError>;

    /// Introspect a multipart upload session: current state, parts already
    /// reported, and — while still resumable — fresh resume URLs for the
    /// parts still missing.
    async fn introspect_multipart(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: Uuid,
    ) -> Result<MultipartStatus, FileStorageError>;

    /// Finalize a multipart upload (assemble all reported parts; idempotent).
    /// Returns [`MultipartCompleteOutcome::Completing`] when another caller
    /// currently holds the completion lease — poll by re-issuing the same
    /// call. `if_match` (optional) is the file's current content `ETag`.
    async fn complete_multipart(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: Uuid,
        if_match: Option<&str>,
    ) -> Result<MultipartCompleteOutcome, FileStorageError>;

    /// Abort a multipart upload and discard all parts.
    async fn abort_multipart(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: Uuid,
    ) -> Result<(), FileStorageError>;

    // ── ownership + backends ─────────────────────────────────────────────────

    /// Transfer ownership of a file to a new owner.
    async fn transfer_ownership(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        new_owner_kind: OwnerKind,
        new_owner_id: Uuid,
    ) -> Result<FileRecord, FileStorageError>;

    /// Migrate a non-versioned file's content to a different storage backend.
    async fn migrate_backend(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        target_backend_id: &str,
    ) -> Result<(), FileStorageError>;

    /// List configured storage backends and their capabilities.
    async fn list_storages(&self, ctx: &SecurityContext) -> Result<Vec<Storage>, FileStorageError>;

    /// Get one configured storage backend by id.
    async fn get_storage(
        &self,
        ctx: &SecurityContext,
        storage_id: &str,
    ) -> Result<Storage, FileStorageError>;

    // ── policy ───────────────────────────────────────────────────────────────

    /// Get the stored (own-level) policy for a scope, if one has been set.
    async fn get_policy(
        &self,
        ctx: &SecurityContext,
        scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<Policy>, FileStorageError>;

    /// Compute the effective (most-restrictive tenant ⊕ user) policy.
    async fn get_effective_policy(
        &self,
        ctx: &SecurityContext,
        user_owner_id: Option<Uuid>,
    ) -> Result<EffectivePolicy, FileStorageError>;

    /// Set (upsert) the policy for a scope.
    async fn put_policy(
        &self,
        ctx: &SecurityContext,
        scope: PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: PolicyBody,
    ) -> Result<Policy, FileStorageError>;

    // ── retention rules ──────────────────────────────────────────────────────

    /// List all retention rules visible to the caller.
    async fn list_retention_rules(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<RetentionRule>, FileStorageError>;

    /// Create a new retention rule.
    async fn create_retention_rule(
        &self,
        ctx: &SecurityContext,
        scope: RetentionScope,
        scope_target_id: Option<Uuid>,
        body: RetentionRuleBody,
    ) -> Result<RetentionRule, FileStorageError>;

    /// Delete a retention rule by id.
    async fn delete_retention_rule(
        &self,
        ctx: &SecurityContext,
        rule_id: Uuid,
    ) -> Result<(), FileStorageError>;
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod api_tests;
