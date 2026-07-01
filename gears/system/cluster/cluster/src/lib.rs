//! # Cluster gear
//!
//! `cluster` is the wiring and lifecycle crate for the cluster coordination
//! gear. It owns the SDK-default backend implementations (leader election,
//! distributed lock, service discovery over cache CAS) that the cluster gear
//! wires up when an operator omits a primitive from their profile config.
//!
//! Consumer gears never instantiate these backends directly — they resolve the
//! cluster primitives via the per-primitive facade resolvers in `cluster-sdk`.
//! Only the cluster gear's wiring layer (this crate) touches the default
//! backend structs.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod defaults;
