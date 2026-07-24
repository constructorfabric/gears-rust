pub(crate) mod metrics;
pub(crate) mod oauth;
pub(crate) mod plugin;
pub(crate) mod proxy;
pub(crate) mod storage;
pub(crate) mod type_provisioning;

use std::time::{SystemTime, UNIX_EPOCH};

/// Current Unix time in whole seconds, saturating at `i64::MAX` and clamping
/// pre-epoch clocks to `0`.
pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
