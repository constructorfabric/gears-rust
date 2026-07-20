// Internal persistence + observability layer. Exposed `pub` only for the
// crate's integration tests (see the crate root for the convention); not
// public API.
#[doc(hidden)]
pub mod metrics;
#[doc(hidden)]
pub mod storage;
