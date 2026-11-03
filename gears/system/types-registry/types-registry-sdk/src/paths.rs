//! The registry's REST base paths, shared by the clients (`rest-client`) and the binding
//! descriptors (`rest-server`).

/// [`PLATFORM`] as a literal, for `concat!`: a client span's `http.route`.
macro_rules! platform_path {
    () => {
        "/types-registry/v2/platform"
    };
}
#[cfg_attr(not(feature = "rest-client"), allow(unused_imports))]
pub(crate) use platform_path;

/// [`TENANT`] as a literal, for `concat!`: a client span's `http.route`.
macro_rules! tenant_path {
    () => {
        "/types-registry/v2"
    };
}
#[cfg_attr(not(feature = "rest-client"), allow(unused_imports))]
pub(crate) use tenant_path;

/// Interim platform API, served on every host (D17).
/// T38 promotes it to `/types-registry/v1/platform` after retiring legacy v1.
pub const PLATFORM: &str = platform_path!();

/// The interim tenant reads; T38 promotes them to `/types-registry/v1`.
pub const TENANT: &str = tenant_path!();
