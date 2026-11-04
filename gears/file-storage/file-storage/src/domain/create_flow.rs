//! Shared `POST /files` orchestration for the REST handler and the SDK local client: single-part
//! vs. merged create+plan multipart path (file and pending version in one transaction).

use uuid::Uuid;

use file_storage_sdk::NewFile;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::multipart::{MultipartPlan, compute_plan};
use crate::domain::multipart_service::MultipartService;
use crate::domain::service::{FileService, UploadTicket};

/// Multipart intent for the merged create+plan path.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct MultipartIntent {
    pub declared_size: u64,
    pub preferred_part_size: Option<u64>,
}

/// Result of [`create_file`]: the single-part ticket, or the file id plus a plan of 2+ parts.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub enum CreateFileOutcome {
    SinglePart(UploadTicket),
    Multipart { file_id: Uuid, plan: MultipartPlan },
}

/// `POST /files`: create a file and presign its first content upload.
///
/// No `multipart` intent (or a one-part plan) yields a single pending version
/// ([`CreateFileOutcome::SinglePart`]). With 2+ parts every check (authorization, MIME and size
/// policy, plan, quota for the declared size) runs before the first write; then the file, its
/// pending version and `file.created` are committed in one transaction, the session is opened for
/// that version, and [`CreateFileOutcome::Multipart`] is returned. A failed session open removes
/// the version and the file again.
///
/// `idempotency_key` is rejected with a `multipart` intent (the stored record fits only a
/// single-part ticket).
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
        if idempotency_key.is_some() {
            return Err(DomainError::validation(
                "idempotency_key",
                "not supported together with the multipart intent block",
            ));
        }
        // One-part plans fall through to the single-part path.
        let (_, planned_parts) = compute_plan(mp.declared_size, mp.preferred_part_size, None)?;
        if planned_parts.len() >= 2 {
            // Everything that can reject the request runs before the first write.
            file_svc.validate_create_inputs(ctx, &new).await?;
            let validated = multipart_svc
                .validate_multipart_plan(
                    ctx,
                    new.owner_id,
                    &new.mime_type,
                    mp.declared_size,
                    mp.preferred_part_size,
                )
                .await?;
            let (file_id, version_id) =
                file_svc.create_file_with_pending_version(ctx, &new).await?;
            let plan = multipart_svc
                .open_session_for_created_file(
                    ctx,
                    new.owner_id,
                    file_id,
                    version_id,
                    &new.mime_type,
                    mp.declared_size,
                    validated,
                    auto_bind,
                )
                .await?;
            return Ok(CreateFileOutcome::Multipart { file_id, plan });
        }
    }

    let ticket = file_svc
        .create_file(ctx, new, idempotency_key, auto_bind)
        .await?;
    Ok(CreateFileOutcome::SinglePart(ticket))
}
