//! Helpers shared across the `repo_impl` split: the repository adapter type,
//! entity/domain converters, and error mapping. Kept in one leaf module so
//! `reads`/`writes` and the parent depend on it one-way (no module cycle).

use std::sync::Arc;

use credstore_sdk::{OwnerId, SharingMode, TenantId, ValueId};
use toolkit_db::DBProvider;
use toolkit_db::secure::ScopeError;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{Fallback, GcEntry, GcReason, SecretRow, SecretStatus};
use crate::infra::canonical_mapping::classify_db_err_to_domain;
use crate::infra::storage::entity;

pub type CredstoreDbProvider = DBProvider<DomainError>;

/// `SeaORM` repository adapter for
/// [`SecretRepo`](crate::domain::secret::repo::SecretRepo).
pub struct SecretRepoImpl {
    pub(crate) db: Arc<CredstoreDbProvider>,
}

impl SecretRepoImpl {
    #[must_use]
    pub fn new(db: Arc<CredstoreDbProvider>) -> Self {
        Self { db }
    }
}

/// Map an entity row to the domain [`SecretRow`].
pub(crate) fn entity_to_model(m: entity::secrets::Model) -> Result<SecretRow, DomainError> {
    let sharing = sharing_from_i16(m.sharing).ok_or_else(|| DomainError::Internal {
        diagnostic: format!(
            "credstore_secrets.sharing out-of-domain value: {}",
            m.sharing
        ),
        cause: None,
    })?;
    let status = SecretStatus::from_smallint(m.status).ok_or_else(|| DomainError::Internal {
        diagnostic: format!("credstore_secrets.status out-of-domain value: {}", m.status),
        cause: None,
    })?;
    let fallback = Fallback::from_smallint(m.fallback).ok_or_else(|| DomainError::Internal {
        diagnostic: format!(
            "credstore_secrets.fallback out-of-domain value: {}",
            m.fallback
        ),
        cause: None,
    })?;
    Ok(SecretRow {
        id: m.id,
        tenant_id: TenantId(m.tenant_id),
        reference: m.reference,
        sharing,
        owner_id: OwnerId(m.owner_id),
        status,
        version: m.version,
        updated_at: m.updated_at,
        // Opaque here: the domain layer resolves the UUID to the type id +
        // traits via the types-registry, so non-catalog types round-trip.
        secret_type_uuid: m.secret_type_uuid,
        expires_at: m.expires_at,
        value_id: m.value_id.map(ValueId),
        value_fp: m.value_fp,
        fp_key_id: m.fp_key_id,
        fallback,
    })
}

/// Map a `credstore_value_gc` entity row to the domain [`GcEntry`].
pub(crate) fn gc_entity_to_model(m: &entity::value_gc::Model) -> Result<GcEntry, DomainError> {
    let reason = GcReason::from_smallint(m.reason).ok_or_else(|| DomainError::Internal {
        diagnostic: format!(
            "credstore_value_gc.reason out-of-domain value: {}",
            m.reason
        ),
        cause: None,
    })?;
    Ok(GcEntry {
        value_id: ValueId(m.value_id),
        tenant_id: TenantId(m.tenant_id),
        reason,
        enqueued_at: m.enqueued_at,
    })
}

/// Map [`SharingMode`] to its `SMALLINT` storage value.
pub(crate) fn sharing_to_i16(s: SharingMode) -> i16 {
    match s {
        SharingMode::Private => 1,
        SharingMode::Tenant => 2,
        SharingMode::Shared => 3,
    }
}

/// Map a `SMALLINT` storage value to [`SharingMode`].
pub(crate) fn sharing_from_i16(v: i16) -> Option<SharingMode> {
    match v {
        1 => Some(SharingMode::Private),
        2 => Some(SharingMode::Tenant),
        3 => Some(SharingMode::Shared),
        _ => None,
    }
}

/// Map a [`ScopeError`] to a [`DomainError`] outside a retry boundary.
pub(super) fn map_scope_err(err: ScopeError) -> DomainError {
    match err {
        ScopeError::Db(db) => classify_db_err_to_domain(db),
        ScopeError::Invalid(msg) => DomainError::Internal {
            diagnostic: format!("scope invalid: {msg}"),
            cause: None,
        },
        ScopeError::TenantNotInScope { .. } => DomainError::AccessDenied { cause: None },
        ScopeError::Denied(msg) => DomainError::Internal {
            diagnostic: format!("unexpected access denied in credstore repo: {msg}"),
            cause: None,
        },
        // `ScopeError` is `#[non_exhaustive]`: variants this gear has no
        // specific answer for (today the graph-query refusals, which it can
        // never trigger) map to an internal error, like `Invalid`.
        other => DomainError::Internal {
            diagnostic: format!("scope invalid: {other}"),
            cause: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::DbErr;
    use time::OffsetDateTime;
    use toolkit_db::secure::ScopeError;
    use uuid::Uuid;

    use super::{
        entity_to_model, gc_entity_to_model, map_scope_err, sharing_from_i16, sharing_to_i16,
    };
    use crate::domain::error::DomainError;
    use crate::domain::secret::model::{Fallback, GcReason, SecretStatus};
    use crate::infra::storage::entity;
    use credstore_sdk::SharingMode;

    /// An `Active`, `Tenant`-shared row with every column in domain.
    fn row() -> entity::secrets::Model {
        entity::secrets::Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            reference: "openai-key".to_owned(),
            sharing: 2,
            owner_id: Uuid::nil(),
            status: 2,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            version: 1,
            secret_type_uuid: Uuid::new_v4(),
            expires_at: None,
            value_id: Some(Uuid::new_v4()),
            value_fp: Some(vec![1, 2, 3]),
            fp_key_id: Some(1),
            fallback: 1,
        }
    }

    fn gc_row() -> entity::value_gc::Model {
        entity::value_gc::Model {
            value_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            reason: 1,
            enqueued_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn sharing_round_trips_through_its_smallint_encoding() {
        for mode in [
            SharingMode::Private,
            SharingMode::Tenant,
            SharingMode::Shared,
        ] {
            assert_eq!(sharing_from_i16(sharing_to_i16(mode)), Some(mode));
        }
        // Codes outside the stored domain have no mode.
        assert_eq!(sharing_from_i16(0), None);
        assert_eq!(sharing_from_i16(4), None);
    }

    #[test]
    fn entity_to_model_maps_every_column_onto_the_domain_row() {
        let m = row();
        let (id, tenant_id, value_id) = (m.id, m.tenant_id, m.value_id);
        let mapped = entity_to_model(m).expect("in-domain row maps");
        assert_eq!(mapped.id, id);
        assert_eq!(mapped.tenant_id.0, tenant_id);
        assert_eq!(mapped.reference, "openai-key");
        assert_eq!(mapped.sharing, SharingMode::Tenant);
        assert_eq!(mapped.status, SecretStatus::Active);
        assert_eq!(mapped.fallback, Fallback::Inherit);
        assert_eq!(mapped.value_id.map(|v| v.0), value_id);
    }

    #[test]
    fn entity_to_model_reports_each_out_of_domain_column_as_internal() {
        for (column, m) in [
            (
                "sharing",
                entity::secrets::Model {
                    sharing: 9,
                    ..row()
                },
            ),
            ("status", entity::secrets::Model { status: 9, ..row() }),
            (
                "fallback",
                entity::secrets::Model {
                    fallback: 9,
                    ..row()
                },
            ),
        ] {
            let err = entity_to_model(m).expect_err("out-of-domain column must be rejected");
            let DomainError::Internal { diagnostic, .. } = err else {
                panic!("{column}: expected Internal");
            };
            assert!(
                diagnostic.contains(column),
                "{column}: diagnostic must name the column, got {diagnostic}"
            );
        }
    }

    #[test]
    fn gc_entity_to_model_maps_the_queue_row_and_rejects_an_unknown_reason() {
        let m = gc_row();
        let (value_id, tenant_id) = (m.value_id, m.tenant_id);
        let mapped = gc_entity_to_model(&m).expect("in-domain row maps");
        assert_eq!(mapped.value_id.0, value_id);
        assert_eq!(mapped.tenant_id.0, tenant_id);
        assert_eq!(mapped.reason, GcReason::Pending);

        let err = gc_entity_to_model(&entity::value_gc::Model {
            reason: 9,
            ..gc_row()
        })
        .expect_err("out-of-domain reason must be rejected");
        assert!(matches!(err, DomainError::Internal { .. }));
    }

    #[test]
    fn maps_each_scope_error_variant() {
        assert!(matches!(
            map_scope_err(ScopeError::Invalid("bad scope")),
            DomainError::Internal { .. }
        ));
        assert!(matches!(
            map_scope_err(ScopeError::TenantNotInScope {
                tenant_id: Uuid::new_v4()
            }),
            DomainError::AccessDenied { .. }
        ));
        assert!(matches!(
            map_scope_err(ScopeError::Denied("not accessible")),
            DomainError::Internal { .. }
        ));
        // Db errors delegate to the classification ladder (CHECK violations
        // are server-side invariants → Internal).
        assert!(matches!(
            map_scope_err(ScopeError::Db(DbErr::Custom(
                "CHECK constraint failed".to_owned()
            ))),
            DomainError::Internal { .. }
        ));
    }
}
