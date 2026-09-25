#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Regression for the FIPS bypass in `TODO.md` item 4: `build_client` MUST
//! fail closed when no process-wide rustls `CryptoProvider` has been
//! installed, rather than falling back to the `clickhouse` crate's own
//! connector.
//!
//! That fallback is the whole bug. `clickhouse::Client::default()` reaches
//! `http_client::default()`, which hardcodes
//! `rustls::crypto::aws_lc_rs::default_provider()` — the non-FIPS AWS-LC
//! build — ignoring whatever `toolkit::bootstrap::init_crypto_provider`
//! installed. A FIPS-configured server would pass every existing check and
//! still send usage records and the database password over non-validated
//! crypto.
//!
//! Lives in its own integration-test binary (a separate process from the
//! in-crate unit tests) because provider installation is process-global and
//! irreversible: any other test that installs one would make the premise here
//! unobservable. Same reasoning as
//! `libs/toolkit-http/tests/no_crypto_provider_fips.rs`.
//!
//! Deliberately NOT `#![cfg(feature = "clickhouse")]` — no Docker and no
//! server are involved, so this runs on every `cargo test`.

use clickhouse_usage_collector_plugin::config::ClickHousePluginConfig;
use clickhouse_usage_collector_plugin::infra::storage::pool::build_client;
use secrecy::SecretString;

#[test]
fn build_client_without_installed_crypto_provider_fails_closed() {
    // Precondition: nothing in this binary has installed a provider. If it
    // had, `new_base_client` would take the success path and the test premise
    // would be invalid rather than merely failing.
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_none(),
        "test precondition: no crypto provider must be installed before this test runs"
    );

    let cfg = ClickHousePluginConfig {
        database_url: SecretString::from("https://chuser:secret@clickhouse.example:8443/usage_db"),
        ..ClickHousePluginConfig::default()
    };
    cfg.validate().expect("https config is valid");

    // `let-else` instead of `.expect_err()` because `clickhouse::Client` does
    // not implement `Debug` (the same reason
    // `libs/toolkit-http/tests/no_crypto_provider_fips.rs` spells it this way).
    let Err(err) = build_client(&cfg) else {
        panic!("build_client must fail closed when no CryptoProvider is installed");
    };

    // Assert on the actionable part of the message: the remedy has to name
    // the bootstrap call, or an operator hitting this has nothing to go on.
    let msg = format!("{err:#}");
    assert!(
        msg.contains("init_crypto_provider"),
        "the error must point at toolkit::bootstrap::init_crypto_provider(), got: {msg}"
    );
}
