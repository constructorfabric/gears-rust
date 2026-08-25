//! Default-configuration tests.
//!
//! The defaults are load-bearing, not cosmetic: an empty `config:` block deploys
//! this plugin entirely on them (see the gear's config gate), so each value is
//! part of the shipped behaviour rather than an internal detail.

use bss_rate_provider_sdk::http_client::build_source_http_client;

use super::EcbPluginConfig;

#[test]
fn defaults_deploy_a_working_ecb_source() {
    let cfg = EcbPluginConfig::default();
    // Must match the core gear's `source_vendor` default, or an otherwise-empty
    // deployment discovers no sources at all on its first tick.
    assert_eq!(cfg.vendor, "cf.bss");
    assert_eq!(cfg.id, "ecb");
    // ECB is the primary source, so it sits ahead of the http-json plugin's
    // default 200 in the composite's fallback order.
    assert_eq!(cfg.priority, 100);
    assert_eq!(
        cfg.base_url,
        "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml"
    );
    assert_eq!(cfg.timeout_ms, 5000);
}

/// `#[tokio::test]`, because building the client registers with the async
/// runtime's reactor — the same thing `init()` does.
#[tokio::test]
async fn the_defaults_pass_this_plugins_own_startup_gate() {
    // `build_source_http_client` rejects a non-https `base_url` and a timeout
    // outside (0, MAX_TIMEOUT_MS]. Defaults that failed either would make an
    // empty `config:` block abort the gear at init instead of deploying.
    let cfg = EcbPluginConfig::default();
    assert!(build_source_http_client(&cfg.base_url, cfg.timeout_ms).is_ok());
}
