//! Shared admission outcomes used by the worker and dry-run paths.

use std::sync::Arc;

use toolkit_db::DBProvider;
use toolkit_db::secure::AccessScope;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::errors::{ItemFailure, WorkerError};
use crate::domain::enums::OperationItemStatus;
use crate::domain::ports::{OperationItemRow, OperationRow, Stores, snapshot_read};

/// What one pass over an operation produced.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationOutcome {
    pub operation_id: Uuid,
    /// Whether redelivery found the operation already terminal.
    pub already_terminal: bool,
    pub items: Vec<ItemOutcome>,
}

/// One candidate's outcome.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemOutcome {
    pub gts_id: String,
    pub status: OperationItemStatus,
    /// The Registry Reference of the admitted entity, on success.
    pub gts_uuid: Option<Uuid>,
    pub resource_version: Option<i64>,
    pub revision_no: Option<i32>,
    pub failure: Option<ItemFailure>,
}

/// Read the operation and its items under one snapshot.
///
/// # Errors
/// [`WorkerError::OperationNotFound`] when the id names no row — an unknown
/// operation is an infrastructure fault, not a candidate outcome.
pub(super) async fn read_operation(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    operation_id: Uuid,
) -> Result<(OperationRow, Vec<OperationItemRow>), WorkerError> {
    let stores_tx = Arc::clone(stores);
    let scope_tx = scope.clone();
    let found = db
        .transaction_with_config(snapshot_read(&db.db()), move |tx| {
            Box::pin(async move {
                let Some(operation) = stores_tx.find_by_id(tx, &scope_tx, operation_id).await?
                else {
                    return Ok(None);
                };
                let items = stores_tx.find_items(tx, &scope_tx, operation_id).await?;
                Ok(Some((operation, items)))
            })
        })
        .await?;
    found.ok_or(WorkerError::OperationNotFound { operation_id })
}

/// Reconstruct a terminal outcome, including Registry References on success.
pub(super) fn stored_outcome(item: &OperationItemRow) -> Result<ItemOutcome, WorkerError> {
    let terminal_success = matches!(
        item.status,
        OperationItemStatus::Succeeded | OperationItemStatus::Unchanged
    );
    // Do not hide corrupt identifiers behind a missing success UUID.
    let gts_uuid = if terminal_success {
        Some(
            gts::GtsId::try_new(&item.gts_id)
                .map_err(|reason| WorkerError::StoredIdentifierUnparsable {
                    item_id: item.id,
                    gts_id: item.gts_id.clone(),
                    reason: reason.to_string(),
                })?
                .to_uuid(),
        )
    } else {
        None
    };
    Ok(ItemOutcome {
        gts_id: item.gts_id.clone(),
        status: item.status,
        gts_uuid,
        resource_version: item.result_resource_version,
        revision_no: item.result_revision_no,
        failure: item.error_payload.as_deref().map(ItemFailure::from_payload),
    })
}
