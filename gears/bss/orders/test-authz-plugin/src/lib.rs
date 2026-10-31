//! Orders test `AuthZ` resolver fixture
//!
//! A declarative policy decision point for **test hosts only** — it is linked into the example
//! server through the explicit Orders test feature, never a production composition.
//!
//! The policy is configuration: `rules` match a subject, a resource type and an action and
//! answer with one constraint per declared path; `unconditional_grants` answer a property-less
//! check with no constraints. Orders owns this fixture, including permissions for the other
//! gears participating in its integration scenarios. The crate is not publishable.
//!
//! ## Configuration
//!
//! ```yaml
//! gears:
//!   bss-orders-test-authz-plugin:
//!     config:
//!       vendor: "constructorfabric"
//!       priority: 10
//!       policy_revision: "suite-r1"
//!       rules:
//!         - id: "buyer"
//!           subject: { id: "11111111-6a88-4768-9dfc-6bcd5187d9ed" }
//!           resource_type: "gts.cf.bss.orders.order.v1~"
//!           actions: ["read"]
//!           paths:
//!             - predicates:
//!                 - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
//!       unconditional_grants: []
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;

pub use gear::OrdersTestAuthZPlugin;
