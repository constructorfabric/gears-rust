#![cfg(all(feature = "bootstrap", feature = "fips"))]

//! Regression for issue #5671: `OoP` bootstrap must install the FIPS crypto provider.
//!
//! Drive `run_oop_with_options` with a non-existent config path so it returns
//! early (after the crypto install) rather than booting a server.

use std::path::PathBuf;

use toolkit::bootstrap::{OopRunOptions, run_oop_with_options};

#[tokio::test]
async fn oop_startup_installs_fips_provider() {
    let opts = OopRunOptions {
        gear_name: "fips-oop-test".to_owned(),
        config_path: Some(PathBuf::from("/nonexistent/5671-oop-fips.yaml")),
        ..OopRunOptions::default()
    };

    let err = run_oop_with_options(opts)
        .await
        .expect_err("startup must fail on a missing config file");
    let msg = err.to_string();
    assert!(
        msg.contains("config file does not exist"),
        "expected a missing-config-file error, got: {msg}"
    );

    let provider = rustls::crypto::CryptoProvider::get_default()
        .expect("OoP bootstrap must install a provider");
    assert!(
        provider.fips(),
        "installed provider must report fips() == true"
    );
}
