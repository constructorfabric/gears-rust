//! `usage-collector-rest-client` crate
//!
//! Provides [`UsageCollectorRestClientModule`] — a `ModKit` module that satisfies
//! the `"usage-collector-client"` dependency when usage is emitted from a **separate**
//! `CyberFabric` binary that must reach the collector over **HTTP/REST** (Scenario C).
//!
//! Each `create_usage_record` exchanges `OAuth2` client credentials via [`AuthNResolverClient`],
//! reads the bearer token from the returned [`SecurityContext`], and POSTs the record
//! to `POST {collector_url}/usage-collector/v1/records`.
//!
//! ## Configuration
//!
//! `collector_url` and `oauth` are required. `scopes` within `oauth` is optional.
//!
//! ```yaml
//! modules:
//!   usage-collector-rest-client:
//!     config:
//!       collector_url: "http://127.0.0.1:8080"
//!       oauth:
//!         client_id: "my-client"
//!         client_secret: "${CLIENT_SECRET}"
//!         scopes: []
//! ```

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod config;
mod infra;
mod module;
