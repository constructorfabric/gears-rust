// Created: 2026-09-06 by Constructor Tech
//! `setting_values` persistence, as far as declaration paths need it.

use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::secure::{DBRunner, SecureUpdateExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::value::ValueRepository;
use crate::infra::storage::entity::setting_value::{self, Entity as ValueEntity};

/// The `SeaORM`-backed [`ValueRepository`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ValueRepo;

#[async_trait]
impl ValueRepository for ValueRepo {
    async fn resync_classification<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        data_classification: &str,
    ) -> Result<u64, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-2
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-4
        // One statement over every row of the declaration, run by the caller
        // inside the transaction that changes the declaration itself. The
        // schema check tying `secret` to `secret_ref` still holds row by row:
        // a re-sync that would contradict it fails here rather than landing.
        let outcome = ValueEntity::update_many()
            .col_expr(
                setting_value::Column::DataClassification,
                Expr::value(data_classification.to_owned()),
            )
            .filter(setting_value::Column::DeclarationId.eq(declaration_id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(|err| DomainError::Internal {
                diagnostic: err.to_string(),
            })?;
        Ok(outcome.rows_affected)
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-4
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-2
    }
}
