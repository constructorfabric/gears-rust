// TEMPORARY(rg-types-in-tr): proves the Phase 1 seed stand-in for P0 T31; drop together with it.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! The linked `gts.cf.core.rg.*` inventory (RG base, tenant, AM user types) is admitted by the
//! real database admission, resolves to the declared effective traits, and re-seeds as a no-op.

use std::sync::Arc;

use gts::{GtsId, GtsTypeId};
use serde_json::{Value, json};
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::{all_inventory_instances, all_inventory_type_schemas};
use toolkit_security::PlatformSecurityContext;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::local_client::LocalClient;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::RegistryService;
use types_registry::infra::outbox::OutboxDispatch;
use types_registry_sdk::{
    EntityField, FieldSelection, PlatformTypesRegistryApiExt, Projection, PublisherContext,
    ReadOptions, ReconcileOptions, ReconcileOutcome, Reconciliation,
};

// Keep the SDK crates linked so their `#[gts_type_schema]` inventory is present.
use account_management_sdk as _;
use resource_group_sdk as _;

mod common;

const RG_PREFIX: &str = "gts.cf.core.rg.";

struct Harness {
    client: LocalClient,
    _outbox: Box<dyn std::any::Any + Send>,
    _dir: Arc<common::TestDir>,
}

async fn harness() -> Harness {
    harness_with(TypesRegistryConfig::default()).await
}

async fn harness_with(config: TypesRegistryConfig) -> Harness {
    let dir = Arc::new(common::TestDir::new("tr-local-client"));
    let dsn = format!(
        "sqlite://{}?mode=rwc&journal_mode=wal",
        dir.path().join("local.db").display()
    );
    let db = common::provider_for_with_outbox(&dsn, 8).await;
    serve_with(db, dir, config).await
}

/// A healthy service with a running outbox over `db`.
async fn serve_with(
    db: Arc<DBProvider<DbError>>,
    dir: Arc<common::TestDir>,
    config: TypesRegistryConfig,
) -> Harness {
    let dispatch = Arc::new(OutboxDispatch::new());
    let service = Arc::new(RegistryService::new(
        db.db(),
        common::stores(),
        RegistrationPolicy::default(),
        config,
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        common::metrics(),
    ));
    let outbox = types_registry::infra::outbox::start(db.db(), &service, &dispatch)
        .await
        .expect("start the admission outbox");
    Harness {
        client: LocalClient::new(Arc::clone(&service)),
        _outbox: Box::new(outbox),
        _dir: dir,
    }
}

fn ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn publisher() -> PublisherContext {
    PublisherContext {
        name: "rg-inventory-seed-test".to_owned(),
        version: "0.1.0".parse().expect("version"),
    }
}

fn inventory_rg_entities() -> Vec<(GtsId, Value)> {
    let mut entries = all_inventory_type_schemas().expect("inventory schemas");
    entries.extend(all_inventory_instances().expect("inventory instances"));
    entries
        .into_iter()
        .filter_map(|entry| {
            let raw = entry
                .get("$id")
                .and_then(Value::as_str)
                .map(|s| s.strip_prefix("gts://").unwrap_or(s))
                .or_else(|| entry.get("gtsId").and_then(Value::as_str))
                .or_else(|| entry.get("id").and_then(Value::as_str))?
                .to_owned();
            raw.starts_with(RG_PREFIX)
                .then(|| (GtsId::try_new(&raw).expect("valid GTS id"), entry))
        })
        .collect()
}

#[tokio::test]
async fn linked_rg_inventory_is_admitted_resolved_and_reseeds_as_noop() {
    // Touch constants so the SDK crates are linked into this binary.
    let tenant = resource_group_sdk::TENANT_RG_TYPE_PATH;
    let user = account_management_sdk::gts::USER_RG_TYPE_CODE;
    let user_group = account_management_sdk::gts::USER_GROUP_RG_TYPE_CODE;

    let h = harness().await;
    let desired = inventory_rg_entities();
    let ids: Vec<&str> = desired.iter().map(|(id, _)| id.as_ref()).collect();
    for want in ["gts.cf.core.rg.type.v1~", tenant, user, user_group] {
        assert!(
            ids.contains(&want),
            "{want} missing from inventory: {ids:?}"
        );
    }

    let cancel = tokio_util::sync::CancellationToken::new();
    let first = PlatformTypesRegistryApiExt::reconcile_entities_and_await(
        &h.client,
        &ctx(),
        &publisher(),
        &desired,
        &ReconcileOptions::default(),
        &cancel,
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "admission rejected the RG inventory: {e:?} / {:?}",
            types_registry_sdk::ReconcileReport::from_canonical(&e)
        )
    });
    let Reconciliation::Reconciled(outcomes) = first else {
        panic!("the first run must submit");
    };
    assert!(
        outcomes
            .values()
            .all(|o| matches!(o, ReconcileOutcome::Admitted)),
        "{outcomes:?}"
    );

    let type_ids: Vec<GtsTypeId> = [tenant, user, user_group]
        .iter()
        .map(|s| GtsTypeId::try_new(s).expect("type id"))
        .collect();
    let read = PlatformTypesRegistryApiExt::batch_get_type_schemas(
        &h.client,
        &ctx(),
        &type_ids,
        ReadOptions {
            projection: Projection::Select(FieldSelection::with(&[
                EntityField::EffectiveTraits,
                EntityField::ResolvedSchema,
            ])),
            fresh: false,
        },
    )
    .await
    .expect("batch read");

    let want_traits = [
        (
            tenant,
            json!({"can_be_root": true, "allowed_parent_types": [tenant], "allowed_membership_types": []}),
        ),
        (
            user,
            json!({"can_be_root": true, "allowed_parent_types": [], "allowed_membership_types": []}),
        ),
        (
            user_group,
            json!({"can_be_root": true, "allowed_parent_types": [user_group], "allowed_membership_types": [user]}),
        ),
    ];
    for (id, want) in want_traits {
        let schema = read[&GtsTypeId::try_new(id).unwrap()]
            .as_ref()
            .unwrap_or_else(|| panic!("{id} not found"));
        assert!(!schema.type_uuid.is_nil(), "{id} has a nil type_uuid");
        assert_eq!(schema.effective_traits.as_ref(), Some(&want), "{id}");
        assert!(
            schema.resolved_schema.is_some(),
            "{id} has no resolved schema"
        );
    }

    let second = PlatformTypesRegistryApiExt::reconcile_entities_and_await(
        &h.client,
        &ctx(),
        &publisher(),
        &desired,
        &ReconcileOptions::default(),
        &cancel,
    )
    .await
    .expect("second reconcile");
    assert!(matches!(second, Reconciliation::UpToDate), "{second:?}");
}
