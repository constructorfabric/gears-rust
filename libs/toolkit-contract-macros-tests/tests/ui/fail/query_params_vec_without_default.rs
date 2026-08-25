//! A `Vec` query field without `#[serde(default)]` must be a compile error.
//!
//! An empty vector emits no key at all, so deserialization would fail with
//! `missing field` — at runtime, on the first request that happens to pass an
//! empty list.

use serde::{Deserialize, Serialize};
use toolkit_contract::QueryParams;

#[derive(Serialize, Deserialize, QueryParams)]
pub struct Filter {
    pub tags: Vec<String>,
}

fn main() {}
