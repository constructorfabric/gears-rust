#![allow(unused_imports, dead_code)]

// Should not trigger DE0708 - non-FIPS hasher
use std::hash::{DefaultHasher, Hasher};

fn main() {}
