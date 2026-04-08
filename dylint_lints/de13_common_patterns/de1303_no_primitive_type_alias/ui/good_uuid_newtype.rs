#![allow(dead_code)]

use uuid::Uuid;

// Good - newtypes provide compile-time type safety
pub struct TenantId(pub Uuid);
pub struct UserId(Uuid);
pub struct GtsId(String);
pub struct Port(u16);

// Good - generic alias (excluded by design)
pub type Wrapper<T> = Vec<T>;

// Good - alias of complex type, not a primitive
pub type JsonValue = serde_json::Value;

// Good - pub(crate) visibility is not flagged (lint only targets fully public aliases)
pub(crate) type InternalId = Uuid;

fn main() {}
