#[cfg(feature = "magika")]
pub mod magika_detector;
pub mod parsers;

#[cfg(feature = "magika")]
pub use magika_detector::MagikaDetector;
pub use parsers::*;
