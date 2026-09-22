//! `SeaORM` specifics of the built-in store: entities, the property-graph
//! declaration, migrations, and shared storage helpers.
//!
//! Nothing above `infra/` *reaches for* any of it, and the architecture lint
//! that forbids raw SQL in gear code extends to entity access outside this
//! component -- but that is a rule, not a compiler guarantee. The chain
//! `pub mod infra` -> `pub mod storage` -> `pub mod entity` leaves these
//! items public to any crate with a dependency edge on this one;
//! `#[doc(hidden)]` keeps them out of rustdoc and does not restrict
//! visibility. Said plainly because the previous wording claimed the stronger
//! thing.

pub mod entity;
pub mod graph;
pub mod migrations;
pub mod odata_mapper;
