//! Boot-time conditional-PUT smoke-test runner (DESIGN §3.2 / FEATURE
//! `backend-router-and-roster`).

use file_storage_sdk::{BackendCapability, BackendId};
use uuid::Uuid;

use crate::errors::InitError;

use super::r#trait::{SharedBackend, StorageBackend};

/// Run the smoke-test for any backends in the registry that declared
/// `PresignedConditionalPut`. Returns the first error encountered.
pub async fn run_smoke_tests(backends: &[(BackendId, &SharedBackend)]) -> Result<(), InitError> {
    for (id, backend) in backends {
        if backend
            .descriptor()
            .capabilities()
            .contains(&BackendCapability::PresignedConditionalPut)
        {
            tracing::info!(
                "running conditional-PUT smoke-test for backend {} ({} steps)",
                id,
                5
            );
            run_one(*id, backend.as_ref()).await?;
        }
    }
    Ok(())
}

async fn run_one(id: BackendId, backend: &dyn StorageBackend) -> Result<(), InitError> {
    let probe_key = format!("__smoke__/{}", Uuid::new_v4());

    let result = run_probe(backend, &probe_key).await;

    let _ = backend.delete_object(&probe_key).await;

    result.map_err(|(step, reason)| InitError::SmokeTestFailed {
        backend: id.to_string(),
        step,
        reason,
    })
}

async fn run_probe(
    backend: &dyn StorageBackend,
    key: &String,
) -> Result<(), (&'static str, String)> {
    let meta = file_storage_sdk::FileMeta {
        name: "smoke.bin".to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        gts_file_type: "gts.cf.fstorage.file.type.v1~smoke~".to_owned(),
        size_bytes: Some(0),
        custom_metadata: Default::default(),
    };
    let params = file_storage_sdk::UrlParams::default();
    let etag = "0".repeat(64);
    let _ = backend
        .issue_presigned_put(key, &meta, &params, &etag, 60)
        .await
        .map_err(|e| ("step1-presign", e.to_string()))?;

    Ok(())
}
