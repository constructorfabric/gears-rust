#![cfg(feature = "integration")]
// The fixture helpers are plain functions, which `allow-expect-in-tests` does
// not reach; a failed expectation there is the test failing, as in the sibling
// PostgreSQL suites.
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The number domain on a real `PostgreSQL`: a number is judged by whether it
//! survives binary64, not by the spelling it arrived in.
//!
//! `PostgreSQL` stores a jsonb number as `numeric` and forgets the spelling, so
//! `1e16` is written as an exponent and read back as `10000000000000000`. A
//! guard that judged the integer spelling by magnitude would accept the write
//! and refuse every later change of the key, which canonicalizes the stored
//! value for its pre-image. `SQLite` keeps the text, so only this backend shows
//! it.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-integration LANE=settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_number_domain_test
//! ```

mod support;

use std::sync::Mutex;

use cf_gears_settings_service::domain::writes::Change;
use serde_json::json;
use settings_service_sdk::SettingKey;
use settings_service_sdk::api::SettingsContributionClient;
use settings_service_sdk::models::{ContributedDeclaration, ScopeClass};
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::MockTypesRegistryClient;
use types_registry_sdk::{GtsTypeId, GtsTypeSchema};
use uuid::Uuid;

use support::platform::{
    DeliveringBroker, Platform, ROOT, RegistryDouble, Replica, STRING_TYPE, pg_fixture,
};

const NUMBER_TYPE: &str = "gts.cf.core.settings.type_number.v1~";

fn type_schema(id: &str, json_type: &str) -> GtsTypeSchema {
    GtsTypeSchema::try_new(
        GtsTypeId::new(id),
        json!({ "$id": format!("gts://{id}"), "type": json_type }),
        None,
        None,
    )
    .expect("a valid root type")
}

/// The usual platform, with a number type beside the string one.
fn platform_with_numbers() -> Platform {
    Platform {
        broker: DeliveringBroker::new(),
        registry: std::sync::Arc::new(RegistryDouble {
            inner: MockTypesRegistryClient::new().with_type_schemas([
                type_schema(STRING_TYPE, "string"),
                type_schema(NUMBER_TYPE, "number"),
            ]),
            registered: Mutex::new(Vec::new()),
        }),
    }
}

fn subscriber() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(ROOT)
        .subject_type("gts.cf.core.security.subject_service.v1~")
        .build()
        .expect("a context")
}

#[tokio::test]
async fn a_number_written_as_an_exponent_is_still_changeable_after_postgres_forgets_the_spelling() {
    let Some(pg) = pg_fixture("number-domain").await else {
        return;
    };
    let platform = platform_with_numbers();
    let replica = Replica::init(&pg, &platform, json!({})).await;
    let key = SettingKey::contributed(
        "cf",
        "demo",
        "numbers",
        "ceiling",
        std::num::NonZeroU32::MIN,
    )
    .expect("a valid key");

    let contributions = replica
        .hub
        .get::<dyn SettingsContributionClient>()
        .expect("the contribution door is bound");
    let mut declaration =
        ContributedDeclaration::new(key.clone(), NUMBER_TYPE, json!(0), ScopeClass::Cascading);
    declaration.requires_step_up = Some(false);
    let result = contributions
        .register_declarations(
            &SecurityContext::anonymous(),
            "number-domain-test".to_owned(),
            vec![declaration],
        )
        .await
        .expect("the declaration registers");
    assert!(result.errors.is_empty(), "refused: {:?}", result.errors);

    replica
        .register(&subscriber(), &key, ROOT)
        .await
        .expect("the subscription registers");

    // An f64 spelling: the guard accepts it, the store keeps `numeric`.
    replica
        .change(&key, ROOT, Change::Set(json!(1e16)), Some("absent"))
        .await
        .expect("1e16 is a double and commits");

    // The activation expectation is the canonical text of what was written,
    // whichever spelling the store hands back.
    let states = pg.states(&key).await;
    assert_eq!(states.len(), 1, "one subscription, one state");
    assert_eq!(states[0].expected_value, "10000000000000000");

    // The next change canonicalizes the stored value, now an integer spelling.
    let tag = pg.row_tag(&key, ROOT).await;
    replica
        .change(&key, ROOT, Change::Set(json!(2)), Some(&tag))
        .await
        .expect("the second write is not refused as VALUE_NOT_CANONICAL");
    assert_eq!(pg.states(&key).await[0].expected_value, "2");
}
