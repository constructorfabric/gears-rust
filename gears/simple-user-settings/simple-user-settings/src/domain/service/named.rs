//! Named settings: keyed JSON values next to the fixed `theme` and `language`.
//!
//! Filed under the same `(user, tenant)` and authorized as the same resource as
//! the fixed fields, so a policy that governs a user's settings governs these
//! too without learning a new resource type.

use std::sync::Arc;

use authz_resolver_sdk::pep::AccessRequest;
use simple_user_settings_sdk::models::NamedSetting;
use toolkit_db::DbError;
use toolkit_db::secure::TxConfig;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::{SETTINGS_RESOURCE, Service, actions};
use crate::domain::error::DomainError;
use crate::domain::fields::SettingsFields;
use crate::domain::repo::SettingsRepository;

/// Longest key accepted, in bytes (keys are ASCII, so also characters).
const MAX_KEY_LEN: usize = 128;

/// Keys are short ASCII identifiers: they end up in URLs and in logs, and a
/// product namespaces them with dots (`portal.projects.view`).
fn validate_key(key: &str) -> Result<(), DomainError> {
    let well_formed = !key.is_empty()
        && key.len() <= MAX_KEY_LEN
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'));
    if well_formed {
        Ok(())
    } else {
        Err(DomainError::validation(
            SettingsFields::KEY,
            format!("must be 1-{MAX_KEY_LEN} characters from A-Z a-z 0-9 . _ - :"),
        ))
    }
}

/// The driver error inside `e`, if any: what tells a transaction that lost a
/// serialization conflict, and should be retried, from one that failed.
fn sea_error(e: &DomainError) -> Option<&sea_orm::DbErr> {
    match e {
        DomainError::Database(DbError::Sea(db_err)) => Some(db_err),
        _ => None,
    }
}

impl<R: SettingsRepository> Service<R> {
    /// Every named setting the caller has, ordered by key.
    pub async fn list_named_settings(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<NamedSetting>, DomainError> {
        let (scope, user_id, tenant_id) = self.named_scope(ctx, actions::GET).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo
            .list_named(&conn, &scope, tenant_id, user_id)
            .await
    }

    /// One named setting, or `None` if the caller has not set it.
    pub async fn get_named_setting(
        &self,
        ctx: &SecurityContext,
        key: &str,
    ) -> Result<Option<NamedSetting>, DomainError> {
        validate_key(key)?;
        let (scope, user_id, tenant_id) = self.named_scope(ctx, actions::GET).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo
            .find_named(&conn, &scope, tenant_id, user_id, key)
            .await
    }

    /// Create or replace one named setting.
    ///
    /// The count bound applies to new keys only, so replacing a setting at the
    /// bound still works. Whether the key is new, how many the caller holds and
    /// the write itself run in one `SERIALIZABLE` transaction, so a concurrent
    /// write or delete cannot change the answer between the check and the
    /// write. A new key over the bound is refused before anything is written;
    /// a transaction that loses a conflict is retried from the start.
    pub async fn put_named_setting(
        &self,
        ctx: &SecurityContext,
        key: &str,
        value: serde_json::Value,
    ) -> Result<NamedSetting, DomainError>
    where
        R: 'static,
    {
        validate_key(key)?;
        let size = serde_json::to_string(&value)
            .map_err(|e| DomainError::internal(format!("named setting value: {e}")))?
            .len();
        if size > self.config.named_value_max_bytes {
            return Err(DomainError::validation(
                SettingsFields::VALUE,
                format!(
                    "exceeds maximum size of {} bytes as JSON",
                    self.config.named_value_max_bytes
                ),
            ));
        }

        let (scope, user_id, tenant_id) = self.named_scope(ctx, actions::UPDATE).await?;
        let limit = self.config.named_settings_per_user;
        let setting = NamedSetting {
            key: key.to_owned(),
            value,
        };

        let repo = Arc::clone(&self.repo);
        self.db
            .db()
            .transaction_with_retry(TxConfig::serializable(), sea_error, move |tx| {
                let repo = Arc::clone(&repo);
                let scope = scope.clone();
                let setting = setting.clone();
                Box::pin(async move {
                    let is_new = repo
                        .find_named(tx, &scope, tenant_id, user_id, &setting.key)
                        .await?
                        .is_none();
                    if is_new {
                        let held = repo.count_named(tx, &scope, tenant_id, user_id).await?;
                        if usize::try_from(held).map_or(true, |held| held >= limit) {
                            return Err(DomainError::LimitReached(format!(
                                "at most {limit} named settings per user; delete one first"
                            )));
                        }
                    }
                    repo.upsert_named(tx, &scope, user_id, tenant_id, setting)
                        .await
                })
            })
            .await
    }

    /// Forget one named setting; `true` if it existed.
    pub async fn delete_named_setting(
        &self,
        ctx: &SecurityContext,
        key: &str,
    ) -> Result<bool, DomainError> {
        validate_key(key)?;
        let (scope, user_id, tenant_id) = self.named_scope(ctx, actions::UPDATE).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo
            .delete_named(&conn, &scope, tenant_id, user_id, key)
            .await
    }

    /// Forget every named setting the caller has, in one statement; the number
    /// removed. The erasure path for a user leaving: no need to list and delete
    /// key by key.
    pub async fn delete_all_named_settings(
        &self,
        ctx: &SecurityContext,
    ) -> Result<u64, DomainError> {
        let (scope, user_id, tenant_id) = self.named_scope(ctx, actions::UPDATE).await?;
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo
            .delete_all_named(&conn, &scope, tenant_id, user_id)
            .await
    }

    /// The caller's key halves and the scope the PDP grants for `action`.
    async fn named_scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
    ) -> Result<(AccessScope, Uuid, Uuid), DomainError> {
        let user_id = ctx.subject_id();
        let tenant_id = ctx.subject_tenant_id();
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SETTINGS_RESOURCE,
                action,
                Some(user_id),
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;
        Ok((scope, user_id, tenant_id))
    }
}
