//! Load the deployment YAML through the server path: products (linked by the
//! `bss-pricing` feature) must get its own database — its init calls
//! `db_required` — and its config section must resolve and validate.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![cfg(feature = "bss-products")]

#[test]
fn e2e_yaml_gives_products_a_database_and_valid_defaults() {
    use std::sync::Arc;
    use toolkit::bootstrap::AppConfig;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/e2e-local.yaml");
    let config = AppConfig::load_or_default(Some(&path)).unwrap();
    let entry: toolkit::bootstrap::config::GearConfig =
        serde_json::from_value(config.gears["bss-products"].clone()).unwrap();
    let database = entry
        .database
        .expect("products calls db_required at init; without a database the server exits");
    let file = serde_json::to_value(&database).unwrap()["file"].clone();
    assert_eq!(
        file,
        serde_json::json!("bss_products.db"),
        "products owns its SQLite file"
    );
    let ctx = toolkit::GearCtx::new(
        "bss-products",
        uuid::Uuid::nil(),
        Arc::new(config),
        Arc::new(toolkit::ClientHub::new()),
        tokio_util::sync::CancellationToken::new(),
    );
    // This is the same typed loader and validation that products init calls.
    let products: bss_products::config::ProductsConfig = ctx.config_or_default().unwrap();
    products.validate().unwrap();
    assert!(products.reference_principals.is_empty());
    assert!(products.resolved_idempotency_retention_hours() > 0);
}
