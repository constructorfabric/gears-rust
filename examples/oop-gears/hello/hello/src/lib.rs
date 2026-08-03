//! Hello Gear
//!
//! A minimal out-of-process gear that exposes a single **public** (`.exposed()`),
//! **anonymous** REST route: `GET /hello/v1/ping`. It has no dependencies and no
//! database — its only purpose is to demonstrate the edge reverse proxy
//! (`cpt-cf-component-gateway-provider`):
//!
//! 1. The gear runs as its own process (the "OoP pod").
//! 2. On startup it self-registers its REST endpoint + OpenAPI spec with the
//!    `DirectoryService`.
//! 3. The built-in `api-gateway` edge discovers it via `ListAllInstances` and
//!    reverse-proxies `GET /hello/v1/ping` to it.

// === MODULE DEFINITION ===
mod gear;
pub use gear::Hello;

// === INTERNAL MODULES ===
#[doc(hidden)]
pub mod api;
