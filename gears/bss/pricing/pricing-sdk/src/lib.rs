//! Infrastructure-free pricing reads and the Products browse contract.
pub mod digest;
pub mod product_catalog;
pub mod read;
pub mod terms;
/// SHA-256 bytes; wire adapters encode these as lowercase hexadecimal.
pub type Digest = [u8; 32];
pub mod acceptance;
pub mod meter_semantics;
