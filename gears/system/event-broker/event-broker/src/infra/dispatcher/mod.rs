//! Optional HTTP gateway (`DESIGN.md:610-612`). Not constructed in
//! standalone mode (`docs/ADR/0007-service-decomposition.md` D4). The
//! routing algorithm is #4345's job; this crate only holds the shells.

pub mod proxy;
pub mod router;
