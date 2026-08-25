//! A nested struct inside a query struct must be a compile error.
//!
//! A query string is a flat list of key/value pairs. The old client flattener
//! silently dropped the outer key, so `{filter: {page: {size: 10}}}` went out
//! as `?size=10` and the server could not decode it either way.

use serde::{Deserialize, Serialize};
use toolkit_contract::QueryParams;

#[derive(Serialize, Deserialize)]
pub struct Page {
    pub size: u32,
}

#[derive(Serialize, Deserialize, QueryParams)]
pub struct Filter {
    pub page: Page,
}

fn main() {}
