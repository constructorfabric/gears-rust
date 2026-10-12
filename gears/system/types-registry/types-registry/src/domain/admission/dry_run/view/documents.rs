//! Current documents, values and artifacts, as the view answers them.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_db::DbTx;
use toolkit_db::secure::{AccessScope, ScopeError};

use super::overlay::CarriedDocument;
use super::{AdmissionView, CurrentKind, unsupported};
use crate::domain::ports::{
    CurrentDocument, CurrentInstanceRow, CurrentInstanceValue, CurrentReadRow, CurrentSchemaCas,
    CurrentSchemaProjection, CurrentTypeSchemaRow, InstanceStore, NewCurrentInstance,
    NewCurrentTypeSchema, NewInstanceRevision, NewRevision, TypeSchemaStore,
};
use crate::domain::selection::FieldSelection;

#[async_trait]
impl TypeSchemaStore for AdmissionView {
    async fn current_documents(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<CurrentDocument>, ScopeError> {
        let stored_ids = self.stored_only(entity_ids, CurrentKind::Schema).await;
        let mut docs = self.base.current_documents(tx, scope, &stored_ids).await?;
        let overlay = self.overlay().await;
        docs.extend(
            entity_ids
                .iter()
                .filter_map(|id| overlay.schema(*id).map(|state| state.document(*id))),
        );
        docs.sort_by_key(|doc| doc.entity_id);
        Ok(docs)
    }

    /// The read path's projected current-state read. Admission compares state through
    /// [`Self::current_schema_projections`] and writes through the two current-state
    /// calls; it never needs a batch of materialized artifacts, and an overlay has
    /// none to give for a candidate whose artifacts this pass only predicted.
    async fn read_current_schemas(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _entity_ids: &[i64],
        _selection: FieldSelection,
    ) -> Result<Vec<CurrentReadRow>, ScopeError> {
        Err(unsupported(
            "an admission view does not serve batched current-state artifacts; \
             the read path does",
        ))
    }

    async fn find_current_schema(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
    ) -> Result<Option<CurrentTypeSchemaRow>, ScopeError> {
        if let Some(state) = self.overlay().await.schema(entity_id) {
            return Ok(Some(state.row(entity_id)));
        }
        if entity_id < 0 {
            return Ok(None);
        }
        self.base.find_current_schema(tx, scope, entity_id).await
    }

    async fn current_schema_projections(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<CurrentSchemaProjection>, ScopeError> {
        let stored_ids = self.stored_only(entity_ids, CurrentKind::Schema).await;
        let mut rows = self
            .base
            .current_schema_projections(tx, scope, &stored_ids)
            .await?;
        let overlay = self.overlay().await;
        rows.extend(
            entity_ids
                .iter()
                .filter_map(|id| overlay.schema(*id).map(|state| state.projection(*id))),
        );
        rows.sort_by_key(|row| row.entity_id);
        Ok(rows)
    }

    async fn insert_schema_revision(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        new: NewRevision,
    ) -> Result<(), ScopeError> {
        self.overlay().await.record_schema_revision(&new);
        Ok(())
    }

    async fn insert_current_schema(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        new: NewCurrentTypeSchema,
    ) -> Result<(), ScopeError> {
        // `ScopeError::Invalid` carries a `&'static str`, so the entity and
        // revision that identify the fault go to the log rather than the caller.
        self.overlay()
            .await
            .set_current_schema(&new, None)
            .map_err(|missing| {
                tracing::error!(%missing, "types_registry admission view rejected a current Type Schema write");
                unsupported(
                    "a current Type Schema was written for a revision this pass never wrote",
                )
            })
    }

    /// CAS against the merged projection; return `false` on a miss.
    /// Refresh may retain the revision number, so carry its authored document from
    /// current state instead of requiring a revision written by this pass.
    async fn update_current_schema(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        new: NewCurrentTypeSchema,
        expected: CurrentSchemaCas,
    ) -> Result<bool, ScopeError> {
        let Some(current) = self
            .current_schema_projections(tx, scope, &[new.entity_id])
            .await?
            .into_iter()
            .next()
        else {
            return Ok(false);
        };
        if current.cas != expected {
            return Ok(false);
        }
        let carried = self
            .current_documents(tx, scope, &[new.entity_id])
            .await?
            .into_iter()
            .next()
            .map(|doc| -> CarriedDocument { Arc::from(doc.raw_schema.as_str()) });
        // Not `is_ok()`: both compare-and-swap misses already returned above, so
        // failing here means the projection has a current row and the document
        // read found none behind it. That is an inconsistent overlay, not a lost
        // CAS, and reporting it as `Ok(false)` would hand the caller a refusal
        // for a candidate that is fine.
        self.overlay()
            .await
            .set_current_schema(&new, carried)
            .map(|()| true)
            .map_err(|missing| {
                tracing::error!(%missing, "types_registry admission view lost the current Type Schema document behind its own projection");
                unsupported("a current Type Schema moved onto a revision with no document to carry")
            })
    }
}

#[async_trait]
impl InstanceStore for AdmissionView {
    async fn current_values(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<CurrentInstanceValue>, ScopeError> {
        let stored_ids = self.stored_only(entity_ids, CurrentKind::Instance).await;
        let mut rows = self.base.current_values(tx, scope, &stored_ids).await?;
        let overlay = self.overlay().await;
        rows.extend(
            entity_ids
                .iter()
                .filter_map(|id| overlay.instance(*id).map(|state| state.value(*id))),
        );
        rows.sort_by_key(|row| row.entity_id);
        Ok(rows)
    }

    async fn find_current_instance(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
    ) -> Result<Option<CurrentInstanceRow>, ScopeError> {
        if let Some(state) = self.overlay().await.instance(entity_id) {
            return Ok(Some(state.row(entity_id)));
        }
        if entity_id < 0 {
            return Ok(None);
        }
        self.base.find_current_instance(tx, scope, entity_id).await
    }

    /// Refused for the reason [`TypeSchemaStore::read_current_schemas`] is.
    async fn read_current_values(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _entity_ids: &[i64],
        _selection: FieldSelection,
    ) -> Result<Vec<CurrentReadRow>, ScopeError> {
        Err(unsupported(
            "an admission view does not serve projected current-state reads; \
             the read path does",
        ))
    }

    async fn insert_instance_revision(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        new: NewInstanceRevision,
    ) -> Result<(), ScopeError> {
        self.overlay().await.record_instance_revision(&new);
        Ok(())
    }

    async fn insert_current_instance(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        new: NewCurrentInstance,
    ) -> Result<(), ScopeError> {
        self.overlay()
            .await
            .set_current_instance(&new)
            .map_err(|missing| {
                tracing::error!(%missing, "types_registry admission view rejected a current Instance write");
                unsupported("a current Instance was written for a revision this pass never wrote")
            })
    }

    /// An Instance has no artifacts, so the pointer only ever moves onto a
    /// revision this pass wrote — there is nothing to carry over.
    async fn update_current_instance(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        new: NewCurrentInstance,
    ) -> Result<bool, ScopeError> {
        if self
            .find_current_instance(tx, scope, new.entity_id)
            .await?
            .is_none()
        {
            return Ok(false);
        }
        // The one miss returned above. An Instance pointer only ever moves onto a
        // revision this pass wrote, so a failure here is that invariant breaking
        // rather than a lost compare-and-swap.
        self.overlay()
            .await
            .set_current_instance(&new)
            .map(|()| true)
            .map_err(|missing| {
                tracing::error!(%missing, "types_registry admission view moved a current Instance onto a revision it never wrote");
                unsupported("a current Instance moved onto a revision this pass never wrote")
            })
    }
}
