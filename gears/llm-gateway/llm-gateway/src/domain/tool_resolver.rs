//! Tool Resolver (DESIGN §3.2, `cpt-cf-llm-gateway-seq-tool-calling-v1`):
//! turns `reference` and `inline_gts` tools into the unified function tool
//! form plugins understand.
//!
//! # Resolution rules
//!
//! - `reference` `{ schema_id }`: the schema is fetched from the
//!   [`ToolSchemaSource`] under the caller's context.
//! - `inline_gts` `{ schema: { json_schema } }`: the inline schema is used as
//!   is.
//! - In both cases every `{"$ref": "gts://<gts id>"}` inside the schema is
//!   replaced by the referenced schema (fetched the same way, recursively;
//!   a cycle is a `validation_error`); `$ref`s that are not `gts://` (e.g.
//!   `#/$defs/x`) are left untouched.
//! - The resulting function tool has `name` = the schema's `title` and
//!   `description` = the schema's `description`; `parameters` = the resolved
//!   schema document; `strict` unset.
//! - `function`, `cf_gears:web_search`, `cf_gears:image_generation` and
//!   provider tools (`Other`) pass through untouched; order is preserved.
//! - An unresolvable schema id fails with `schema_not_found`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use llm_gateway_sdk::{FunctionTool, LlmGatewayError, Tool};
use serde_json::Value;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use super::ports::ToolSchemaSource;

const GTS_REF_PREFIX: &str = "gts://";

/// Function names are `^[a-zA-Z0-9_-]+$`, at most 64 characters.
const MAX_FUNCTION_NAME_LEN: usize = 64;

/// Upper bound on `gts://` references expanded for one request. Guards against
/// schema graphs whose expansion grows exponentially without being cyclic.
const MAX_REF_EXPANSIONS: usize = 256;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Per-`resolve` state: fetched schemas (each id is fetched once) and the
/// expansion and byte budgets of the whole request.
struct Session {
    fetched: HashMap<String, Value>,
    expansions: usize,
    bytes: usize,
    max_bytes: usize,
}

impl Session {
    fn new(max_bytes: usize) -> Self {
        Self {
            fetched: HashMap::new(),
            expansions: 0,
            bytes: 0,
            max_bytes,
        }
    }

    /// Charge the serialized size of `schema` against the byte budget.
    fn charge(&mut self, schema: &Value) -> Result<(), LlmGatewayError> {
        let size = serde_json::to_vec(schema).map_or(usize::MAX, |bytes| bytes.len());
        self.bytes = self.bytes.saturating_add(size);
        if self.bytes > self.max_bytes {
            return Err(LlmGatewayError::validation_param(
                "tool schema is too large",
                "tools",
            ));
        }
        Ok(())
    }
}

/// Resolves GTS tools through a [`ToolSchemaSource`].
#[domain_model]
pub struct ToolResolver {
    source: Arc<dyn ToolSchemaSource>,
    max_schema_bytes: usize,
}

fn sanitize_chars(raw: &str) -> Vec<char> {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A function name from a schema title: characters outside `[a-zA-Z0-9_-]`
/// become `_`, truncated (head kept) to 64 characters.
fn sanitize_name(raw: &str) -> String {
    sanitize_chars(raw)
        .into_iter()
        .take(MAX_FUNCTION_NAME_LEN)
        .collect()
}

/// A function name derived from a GTS id: as [`sanitize_name`], but a long id
/// keeps its discriminating tail (the last 64 characters), since GTS ids
/// share long common prefixes.
fn sanitize_id(raw: &str) -> String {
    let chars = sanitize_chars(raw);
    let skip = chars.len().saturating_sub(MAX_FUNCTION_NAME_LEN);
    chars.into_iter().skip(skip).collect()
}

fn gts_ref(object: &serde_json::Map<String, Value>) -> Option<&str> {
    object
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix(GTS_REF_PREFIX))
}

impl ToolResolver {
    /// Resolver over `source`.
    #[must_use]
    pub fn new(source: Arc<dyn ToolSchemaSource>, max_schema_bytes: usize) -> Self {
        Self {
            source,
            max_schema_bytes,
        }
    }

    /// Resolve `tools` into unified form; see the module docs.
    ///
    /// # Errors
    ///
    /// `schema_not_found` naming the schema id when a GTS schema cannot be
    /// resolved; `validation_error` for a reference cycle or a schema that
    /// cannot be turned into a function tool.
    pub async fn resolve(
        &self,
        ctx: &SecurityContext,
        tools: Vec<Tool>,
    ) -> Result<Vec<Tool>, LlmGatewayError> {
        let mut session = Session::new(self.max_schema_bytes);
        let mut resolved = Vec::with_capacity(tools.len());
        for tool in tools {
            let function = match tool {
                Tool::Reference(reference) => {
                    let schema_id = reference.schema_id;
                    let schema = self.fetch(ctx, &mut session, &schema_id).await?;
                    let mut stack = vec![schema_id.clone()];
                    session.charge(&schema)?;
                    let schema = self.inline(ctx, &mut session, schema, &mut stack).await?;
                    Self::function_tool(schema, Some(&schema_id))?
                }
                Tool::InlineGts(inline) => {
                    let mut stack = Vec::new();
                    session.charge(&inline.schema.json_schema)?;
                    let schema = self
                        .inline(ctx, &mut session, inline.schema.json_schema, &mut stack)
                        .await?;
                    let id = schema
                        .get("$id")
                        .and_then(Value::as_str)
                        .map(|id| id.strip_prefix(GTS_REF_PREFIX).unwrap_or(id).to_owned());
                    Self::function_tool(schema, id.as_deref())?
                }
                other => {
                    resolved.push(other);
                    continue;
                }
            };
            resolved.push(Tool::Function(function));
        }
        Ok(resolved)
    }

    async fn fetch(
        &self,
        ctx: &SecurityContext,
        session: &mut Session,
        schema_id: &str,
    ) -> Result<Value, LlmGatewayError> {
        if let Some(schema) = session.fetched.get(schema_id) {
            return Ok(schema.clone());
        }
        let schema = self.source.schema(ctx, schema_id).await?;
        session.fetched.insert(schema_id.to_owned(), schema.clone());
        Ok(schema)
    }

    /// Replace every `{"$ref": "gts://<id>"}` inside `schema`, recursively.
    /// `stack` holds the ids being expanded: meeting one again is a cycle.
    fn inline<'a>(
        &'a self,
        ctx: &'a SecurityContext,
        session: &'a mut Session,
        schema: Value,
        stack: &'a mut Vec<String>,
    ) -> BoxFuture<'a, Result<Value, LlmGatewayError>> {
        Box::pin(async move {
            match schema {
                Value::Object(mut object) => {
                    if let Some(id) = gts_ref(&object).map(ToOwned::to_owned) {
                        if stack.contains(&id) {
                            return Err(LlmGatewayError::validation_param(
                                format!("cyclic schema reference: {id}"),
                                "tools",
                            ));
                        }
                        session.expansions += 1;
                        if session.expansions > MAX_REF_EXPANSIONS {
                            return Err(LlmGatewayError::validation_param(
                                "too many schema references in one tool",
                                "tools",
                            ));
                        }
                        let target = self.fetch(ctx, session, &id).await?;
                        session.charge(&target)?;
                        stack.push(id);
                        let expanded = self.inline(ctx, session, target, stack).await?;
                        stack.pop();
                        object.remove("$ref");
                        return Ok(match expanded {
                            Value::Object(mut merged) => {
                                // Sibling keywords of the `$ref` stay in force.
                                for (key, value) in object {
                                    merged.insert(key, value);
                                }
                                Value::Object(merged)
                            }
                            other => other,
                        });
                    }
                    let mut out = serde_json::Map::with_capacity(object.len());
                    for (key, value) in object {
                        out.insert(key, self.inline(ctx, session, value, stack).await?);
                    }
                    Ok(Value::Object(out))
                }
                Value::Array(items) => {
                    let mut out = Vec::with_capacity(items.len());
                    for item in items {
                        out.push(self.inline(ctx, session, item, stack).await?);
                    }
                    Ok(Value::Array(out))
                }
                scalar => Ok(scalar),
            }
        })
    }

    /// The unified function tool for a resolved `schema`: `name` from the
    /// schema `title` (else the sanitized GTS id `fallback_id`).
    fn function_tool(
        schema: Value,
        fallback_id: Option<&str>,
    ) -> Result<FunctionTool, LlmGatewayError> {
        let name = schema
            .get("title")
            .and_then(Value::as_str)
            .map(sanitize_name)
            .or_else(|| fallback_id.map(sanitize_id))
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                LlmGatewayError::validation_param("tool schema has no title", "tools")
            })?;
        let description = schema
            .get("description")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Ok(FunctionTool {
            name,
            description,
            parameters: Some(schema),
            strict: None,
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "tool_resolver_tests.rs"]
mod tool_resolver_tests;
