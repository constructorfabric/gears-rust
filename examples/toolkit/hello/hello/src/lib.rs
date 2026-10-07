//! Hello - minimal self-contained REST `OoP` demo gear.
//!
//! Exposes a single anonymous, externally-exposed route:
//!
//! ```text
//! GET /hello/v1/ping  ->  { "message": "pong", "served_by": "hello-oop (pid N)" }
//! ```
//!
//! It has no dependencies on other gears, so it can run as its own `OoP` unit — a
//! host worker in Profile 2 or a pod in Profile 3: it registers its REST
//! endpoint with flight-control's `DirectoryService`, and the api-gateway edge
//! reverse-proxies external `/hello/v1/ping` requests to it. `served_by` reports
//! the serving process id so a caller can confirm the request was proxied to the
//! `OoP` unit.

mod gear;
pub use gear::Hello;

#[doc(hidden)]
pub mod api;
