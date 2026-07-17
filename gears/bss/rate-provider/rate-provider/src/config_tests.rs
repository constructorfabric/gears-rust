//! Default-configuration tests.
//!
//! An empty `config:` block deploys the core gear entirely on these defaults, and
//! three of the four have to agree with something outside this crate — so they are
//! part of the contract, not internal detail.

use super::RateProviderConfig;

#[test]
fn defaults_line_up_with_the_ledger_and_the_source_plugins() {
    let cfg = RateProviderConfig::default();
    // `vendor` is what the ledger's `fx.provider_vendor` must match to resolve
    // this composite; `source_vendor` is what each source plugin's own `vendor`
    // must match for this gear to compose it. Both sides ship "cf.bss", so an
    // all-defaults deployment wires up end to end with no configuration at all.
    assert_eq!(cfg.vendor, "cf.bss");
    assert_eq!(cfg.source_vendor, "cf.bss");
    assert_eq!(cfg.priority, 100);
    assert_eq!(cfg.id, "bss-rate-provider");
}

#[test]
fn the_default_id_is_not_the_ledgers_unconfigured_sentinel() {
    // The ledger treats `provider_id() == "none"` as "no adapter wired" and stays
    // silent on a fetch failure instead of alarming. A default of "none" here
    // would therefore turn every outage of a *deployed* adapter into silence.
    assert_ne!(RateProviderConfig::default().id, "none");
}
