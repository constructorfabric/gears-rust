//! The engine generations compiled into this build (DESIGN §4.1, ADR-0006).
//!
//! Each generation is a frozen crate of its own with its vendored evaluation code; this module
//! only names them. A generation stays here for as long as any retained result references it;
//! removing one needs an ADR and proof that no retained result does (DESIGN §4.12).

/// Generation 1: the first evaluation semantics (feature 02).
pub use bss_rating_engine_gen_1 as gen_1;

/// The generation new work is evaluated with.
pub use bss_rating_engine_gen_1 as current;
