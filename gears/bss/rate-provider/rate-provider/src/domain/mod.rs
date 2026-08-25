//! Domain layer — the ordered-fallback policy over the source ports, with no
//! transport, registry, or service-locator type in its signatures.
//!
//! Only [`composite::CompositeRateProvider`] lives here. Source *discovery* is
//! infrastructure (it talks to the types-registry over the wire and resolves
//! `ClientHub` entries), so it sits in `crate::infra::discovery` instead.

pub mod composite;
