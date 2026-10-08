//! Load the deployment YAML through the server path: the approvals inbox has no
//! database, serves only with its config object, and asks pricing and products.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![cfg(feature = "bss-approvals")]

#[test]
fn e2e_yaml_configures_the_inbox_over_pricing_and_products_without_a_database() {
    use std::sync::Arc;
    use toolkit::bootstrap::AppConfig;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/e2e-local.yaml");
    let config = AppConfig::load_or_default(Some(&path)).unwrap();
    let entry: toolkit::bootstrap::config::GearConfig =
        serde_json::from_value(config.gears["bss-approvals"].clone()).unwrap();
    assert!(
        entry.database.is_none(),
        "the inbox keeps no units of its own"
    );
    let ctx = toolkit::GearCtx::new(
        "bss-approvals",
        uuid::Uuid::nil(),
        Arc::new(config),
        Arc::new(toolkit::ClientHub::new()),
        tokio_util::sync::CancellationToken::new(),
    );
    // The same typed loader the gear's init calls; without the object it does not serve.
    let inbox: bss_approvals::config::ApprovalsConfig = ctx.config().unwrap();
    assert_eq!(inbox.sources, ["pricing", "products"]);
}
