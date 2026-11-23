//! The quota catalog's in-process client (P-D-272): it answers what the REST reads answer, as its
//! caller, page by page with the same cursor, and another tenant's version is not found.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::sync::Arc;

use super::*;
use crate::api::rest::quota::dto::ProductsQuotaDefinition;
use crate::quota_test_support::{
    QuotaApp, QuotaFakes, quota_app_for, seed_sku_version, vcpu_record_numbered,
};
use crate::test_support::{body_json, get, tenant_user};
use bss_products_sdk::quota::catalog::QuotaAvailabilityFilter;
use serde_json::Value;

fn client(q: &QuotaApp) -> LocalQuotaCatalogClient {
    LocalQuotaCatalogClient::new(
        q.state.clone(),
        Arc::new(PolicyEnforcer::new(q.pdp.clone())),
    )
}

/// The REST mirror of an SDK definition, as JSON.
fn rest_of(definition: QuotaDefinition) -> Value {
    serde_json::to_value(ProductsQuotaDefinition::from(definition)).unwrap()
}

fn request(sku: Uuid, cursor: Option<String>) -> QuotaDefinitionListRequest {
    QuotaDefinitionListRequest {
        sku_id: sku,
        sku_version: QuotaVersion::new(1).unwrap(),
        subject_type_ref: None,
        resource_type_ref: None,
        profile_id: None,
        locale: Some("ru".to_owned()),
        availability: QuotaAvailabilityFilter::All,
        limit: Some(1),
        cursor,
    }
}

#[tokio::test]
async fn the_sdk_client_answers_what_rest_answers() {
    let tenant = Uuid::new_v4();
    let q = quota_app_for(&[tenant], QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    for i in 0..2 {
        q.approved(tenant, vcpu_record_numbered(i), sku, 1).await;
    }
    let caller = tenant_user(tenant);
    let sdk = client(&q);
    let first = sdk
        .list_quota_definitions(&caller, request(sku, None))
        .await
        .unwrap();
    let filter = format!("sku_id eq {sku} and sku_version eq '1'")
        .replace(' ', "%20")
        .replace('\'', "%27");
    let uri = format!(
        "/bss-products/v1/quota-definitions?$filter={filter}&availability=all&locale=ru&limit=1"
    );
    let rest = body_json(get(&q.app, tenant, &uri).await).await;
    assert_eq!(first.items.len(), 1);
    assert_eq!(rest_of(first.items[0].clone()), rest["items"][0]);
    assert_eq!(
        first.next_cursor.as_deref(),
        rest["page_info"]["next_cursor"].as_str()
    );
    // The SDK's cursor walks on in the SDK and in REST alike.
    let second = sdk
        .list_quota_definitions(&caller, request(sku, first.next_cursor.clone()))
        .await
        .unwrap();
    let rest_second = body_json(
        get(
            &q.app,
            tenant,
            &format!("{uri}&cursor={}", first.next_cursor.unwrap()),
        )
        .await,
    )
    .await;
    assert_eq!(rest_of(second.items[0].clone()), rest_second["items"][0]);
    assert_eq!(second.next_cursor, None);
    let definition = second.items[0].clone();
    let one = sdk
        .get_quota_definition(
            &caller,
            definition.definition_id,
            definition.version,
            Some("ru"),
        )
        .await
        .unwrap();
    assert_eq!(one, definition);
    let rest_one = body_json(
        get(
            &q.app,
            tenant,
            &format!(
                "/bss-products/v1/quota-definitions/{}/versions/1?locale=ru",
                definition.definition_id
            ),
        )
        .await,
    )
    .await;
    assert_eq!(rest_of(one), rest_one);
}

#[tokio::test]
async fn the_sdk_client_refuses_another_tenants_version() {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let q = quota_app_for(&[tenant, other], QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let (d, version) = q.approved(tenant, vcpu_record_numbered(1), sku, 1).await;
    let sdk = client(&q);
    assert!(
        sdk.get_quota_definition(&tenant_user(tenant), d, version, None)
            .await
            .is_ok()
    );
    let refused = sdk
        .get_quota_definition(&tenant_user(other), d, version, None)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, CanonicalError::NotFound { .. }),
        "{refused:?}"
    );
    let page = sdk
        .list_quota_definitions(&tenant_user(other), request(sku, None))
        .await
        .unwrap();
    assert!(page.items.is_empty(), "another tenant lists none");
}
