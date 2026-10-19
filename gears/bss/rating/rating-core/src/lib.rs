//! BSS Rating evaluation core (DESIGN §1.3, ADR-0002).
//!
//! The deterministic, I/O-free half of Rating: the evaluation steps in their fixed order (bind,
//! meter, model, floor, guards), the engine generations and the exact arithmetic over
//! `ExactMoney` and `ExactQuantity`. Feature 02 (evaluation core) builds it; the foundation only
//! creates the crate and the rule that keeps it pure.
//!
//! **Purity.** No tokio, sea-orm, http or `ClientHub` may enter this crate's normal dependency tree,
//! directly or transitively: an evaluation must give a byte-identical result when it is replayed
//! years later, and nothing that reads a clock, a database or the network may take part in it. CI
//! checks the tree with `make check-pure-crates` (`tools/scripts/pure_crates.toml`). This is also
//! why the core does not depend on `bss-pricing-sdk`, which pulls `http` through
//! `toolkit-contract`: the pricing adapter in the `rating` gear maps Pricing's types into the
//! core's own frozen input types.
#![forbid(unsafe_code)]
