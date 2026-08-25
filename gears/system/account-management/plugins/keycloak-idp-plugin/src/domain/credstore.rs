//! `CredStore` seams — ctx-baked-in wrappers ensuring call sites cannot
//! accidentally pass AM-forwarded ctx to `CredStore`.
//!
//! [`CredStoreReader`] is the canonical read-only path (DESIGN "Component Model").
//! [`CredStoreWriter`] wraps the unified `credstore_sdk::CredStoreClientV1`
//! (the gateway client resolved from the `ClientHub`) for `put`/`delete`.
//!
//! Reader and writer are deliberately separate types — not one `CredStore` —
//! so that read-only call sites cannot accidentally reach a mutation method
//! (DESIGN "Component Model").

pub mod reader;
pub mod writer;

pub use reader::CredStoreReader;
pub use writer::CredStoreWriter;
