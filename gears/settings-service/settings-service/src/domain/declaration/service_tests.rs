//! Tests for the trait set a declaration read attaches.

use std::sync::Arc;

use serde_json::{Value, json};
use types_registry_sdk::testing::MockTypesRegistryClient;
use types_registry_sdk::{GtsTypeId, GtsTypeSchema, TypesRegistryClient};
use uuid::Uuid;

use super::DeclarationService;
use crate::domain::declaration::{Declaration, DeclarationStatus};
use crate::infra::storage::declaration_repo::DeclarationRepo;

const PLAIN: &str = "gts.cf.core.settings.type_plain_text.v1~";
const SECRET: &str = "gts.cf.core.settings.type_secret_string.v1~";
const UNKNOWN: &str = "gts.cf.core.settings.type_never_registered.v1~";

fn type_schema(id: &str, body: Value) -> GtsTypeSchema {
    GtsTypeSchema::try_new(GtsTypeId::new(id), body, None, None)
        .expect("fixture schema is a valid root type")
}

/// A registry that knows a type without traits and a type with one.
fn service() -> DeclarationService<DeclarationRepo> {
    let types: Arc<dyn TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new().with_type_schemas([
            type_schema(
                PLAIN,
                json!({ "$id": format!("gts://{PLAIN}"), "type": "string" }),
            ),
            type_schema(
                SECRET,
                json!({
                    "$id": format!("gts://{SECRET}"),
                    "type": "string",
                    "x-gts-traits": { "secret": true }
                }),
            ),
        ]));
    DeclarationService::new(DeclarationRepo, types)
}

fn declaration(value_type_id: &str) -> Declaration {
    Declaration {
        id: Uuid::new_v4(),
        key: "gts.cf.core.settings.setting_type.v1~acme.settings.network.proxy.v1~".to_owned(),
        leaf_slug: "proxy".to_owned(),
        value_type_id: value_type_id.to_owned(),
        category_id: Uuid::nil(),
        scope_class: "local".to_owned(),
        mode: "standard".to_owned(),
        status: DeclarationStatus::Active,
        domain_affinity: None,
        licence_feature: None,
        owner_module: None,
        display_name: None,
        description: None,
        default_value: json!(""),
        has_secret_trait: false,
        data_classification: "public".to_owned(),
        requires_step_up: false,
        anonymous_exposable: false,
        source: "admin_authored".to_owned(),
        last_change_at: time::OffsetDateTime::UNIX_EPOCH,
        updated_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn the_registry_answers_null_for_a_type_without_traits() {
    // The premise of the two tests below: the merge is `null`, not `{}`, for a
    // type that declares no traits. Should the registry start answering `{}`,
    // the normalization is merely redundant.
    let plain = type_schema(
        PLAIN,
        json!({ "$id": format!("gts://{PLAIN}"), "type": "string" }),
    );
    assert_eq!(plain.effective_traits(), Value::Null);
}

#[tokio::test]
async fn a_type_without_traits_renders_an_empty_object_not_null() {
    let rendered = service().render_one(declaration(PLAIN)).await;
    assert_eq!(rendered.traits, json!({}));
}

#[tokio::test]
async fn a_type_the_registry_does_not_know_renders_an_empty_object() {
    let rendered = service().render_one(declaration(UNKNOWN)).await;
    assert_eq!(rendered.traits, json!({}));
}

#[tokio::test]
async fn a_page_carries_an_object_for_every_declaration() {
    let rendered = service()
        .render(vec![
            declaration(PLAIN),
            declaration(SECRET),
            declaration(UNKNOWN),
        ])
        .await;
    let traits: Vec<Value> = rendered.into_iter().map(|r| r.traits).collect();
    assert_eq!(traits, [json!({}), json!({ "secret": true }), json!({})]);
}
