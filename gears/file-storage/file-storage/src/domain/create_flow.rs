//! Shared `POST /files` orchestration (upload-flow redesign).
//!
//! Deciding between the ordinary single-part path and the merged
//! create+plan multipart path — and running the same orphan-file
//! compensation when a multipart initiate fails after the bare file row was
//! already committed — is more than a single service call, so it doesn't
//! belong duplicated in both the REST handler
//! (`api::rest::handlers::create_file`) and the SDK local client
//! (`FileStorageLocalClient::create_file`). This module is the one place
//! that logic lives; both callers go through [`create_file`].

use uuid::Uuid;

use file_storage_sdk::NewFile;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::multipart::{MultipartPlan, compute_plan};
use crate::domain::multipart_service::MultipartService;
use crate::domain::service::{FileService, UploadTicket};

/// Multipart intent for the merged create+plan path (mirrors the control
/// API's `multipart` request block and the SDK's `MultipartIntent`).
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct MultipartIntent {
    pub declared_size: u64,
    pub preferred_part_size: Option<u64>,
}

/// Result of [`create_file`]: either the ordinary single-part ticket, or —
/// when `multipart` was given and the server-computed plan has two or more
/// parts — the new file's id plus the full parts plan.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub enum CreateFileOutcome {
    SinglePart(UploadTicket),
    Multipart { file_id: Uuid, plan: MultipartPlan },
}

/// `POST /files`: create a file and presign its first content upload.
///
/// * No `multipart` intent (or one whose computed plan collapses to one
///   part): creates the file with a single pending version and returns
///   [`CreateFileOutcome::SinglePart`] — same as [`FileService::create_file`].
/// * `multipart` intent with a plan of two or more parts: creates the bare
///   file row (no pending version — the multipart initiate registers its
///   own) and returns [`CreateFileOutcome::Multipart`]. A failed initiate is
///   compensated synchronously (deletes the now-orphaned bare file row)
///   before the original error is propagated — see
///   [`FileService::compensate_failed_multipart_initiate`]'s doc.
///
/// `idempotency_key` is rejected together with a `multipart` intent (the
/// stored idempotency record only fits a single-part ticket).
pub async fn create_file(
    file_svc: &FileService,
    multipart_svc: &MultipartService,
    ctx: &SecurityContext,
    new: NewFile,
    idempotency_key: Option<String>,
    auto_bind: bool,
    multipart: Option<MultipartIntent>,
) -> Result<CreateFileOutcome, DomainError> {
    if let Some(mp) = &multipart {
        // The idempotency record stores a single-part replay ticket; a
        // multipart plan does not fit that contract — reject rather than
        // silently ignoring one of the two.
        if idempotency_key.is_some() {
            return Err(DomainError::validation(
                "idempotency_key",
                "not supported together with the multipart intent block",
            ));
        }
        // Same plan computation the standalone initiate path runs — decides
        // up front whether this is a real (≥2 parts) multipart upload.
        // One-part plans fall through to the ordinary single-part path below.
        let (_, planned_parts) = compute_plan(mp.declared_size, mp.preferred_part_size, None)?;
        if planned_parts.len() >= 2 {
            let mime_type = new.mime_type.clone();
            // Create the file row only (no single-part pending version — the
            // multipart initiate registers its own; the old flow's abandoned
            // presign orphan disappears).
            let file_id = file_svc.create_file_bare(ctx, new).await?;
            let plan = match multipart_svc
                .initiate_multipart_upload(
                    ctx,
                    file_id,
                    &mime_type,
                    mp.declared_size,
                    mp.preferred_part_size,
                    auto_bind,
                )
                .await
            {
                Ok(plan) => plan,
                Err(e) => {
                    // A capability rejection or backend-side initiation error
                    // here would otherwise leave the bare file just created
                    // above as a version-less orphan that nothing reclaims
                    // until the background sweep's `sweep_versionless_files`
                    // phase ages it past `orphan_grace_secs` — see
                    // `FileService::compensate_failed_multipart_initiate`'s
                    // own doc. Compensate synchronously instead of waiting on
                    // that sweep; the ORIGINAL initiate error is still what
                    // the caller sees.
                    file_svc
                        .compensate_failed_multipart_initiate(ctx, file_id)
                        .await;
                    return Err(e);
                }
            };
            return Ok(CreateFileOutcome::Multipart { file_id, plan });
        }
    }

    let ticket = file_svc
        .create_file(ctx, new, idempotency_key, auto_bind)
        .await?;
    Ok(CreateFileOutcome::SinglePart(ticket))
}

#[cfg(test)]
#[path = "create_flow_tests.rs"]
mod create_flow_tests;
