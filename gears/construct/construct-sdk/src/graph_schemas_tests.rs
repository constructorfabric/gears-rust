//! Test support shared by the type tests: resolves graph storage's base
//! schemas for a validator. The graph-storage SDK embeds the documents it
//! publishes, so the tests read them without a path into another gear's tree.

use jsonschema::{Retrieve, Uri};
use serde_json::Value;

/// A `jsonschema` retriever over `graph_storage_sdk::gts::BASE_SCHEMAS`.
pub struct GraphStorageSchemas;

impl Retrieve for GraphStorageSchemas {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        graph_storage_sdk::gts::BASE_SCHEMAS
            .iter()
            .find(|(id, _)| format!("gts://{id}") == uri.as_str())
            .map(|(_, text)| serde_json::from_str(text).expect("a JSON schema"))
            .ok_or_else(|| format!("unresolved schema reference `{uri}`").into())
    }
}
