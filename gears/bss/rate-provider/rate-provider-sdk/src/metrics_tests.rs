//! Metrics smoke test: recording against the global (no-op) meter must not panic.

use super::{FetchMetrics, OtelFetchMetrics};

#[test]
fn records_against_global_meter_without_panic() {
    let m = OtelFetchMetrics::from_global();
    m.observe_fetch("ecb", 0.12);
    m.incr_fetch_error("ecb", "unreachable");
    m.incr_upstream_status("ecb", 503);
    m.set_last_success("ecb", 1_753_000_000);
    m.observe_rates_returned("ecb", 31);
}
