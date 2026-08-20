//! Contract tests via the AM SDK plugin-conformance harness
//! (DESIGN "Verification Architecture", SDK contract level).
//!
//! As of commit `1fdc57b` (Phase 16b), the upstream
//! `account-management-sdk` crate does not expose a
//! conformance harness for the `IdpPluginClient` trait — `src/idp.rs`
//! defines the trait + DTOs, and the SDK's only `*_tests.rs` modules
//! cover DTO (de)serialisation, not plugin behaviour. When the harness
//! lands upstream (tracked separately — see the follow-up
//! ticket), replace this placeholder with a wired test, roughly:
//!
//! ```ignore
//! #[tokio::test]
//! async fn am_sdk_idp_plugin_conformance() {
//!     let plugin = build_test_plugin().await;
//!     account_management_sdk::idp::conformance::run(&plugin)
//!         .await
//!         .expect("conformance");
//! }
//! ```
//!
//! `build_test_plugin()` will most likely reuse the testcontainer
//! Keycloak scaffolding from `tests/integration_keycloak.rs` (Phase
//! 16b). If the harness ships as a non-Docker pure-trait fixture, the
//! call site stays gate-free; otherwise gate behind `#[ignore]` and
//! run on the Docker-enabled CI lane.
//!
//! Until then, the unit tests under `src/` and the integration tests
//! under `tests/integration_keycloak.rs` cover the trait surface
//! directly (DESIGN "Verification Architecture", unit and integration levels).

#[test]
fn placeholder_until_am_sdk_conformance_harness_lands() {
    // No-op test to ensure the file compiles and is discovered by
    // `cargo test -p keycloak-idp-plugin`. Replace with the wired
    // conformance call when the upstream harness is published.
}
