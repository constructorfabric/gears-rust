// TODO: DE0301 - domain still uses infra types (modkit_db::DbError, SeaORM entities, etc.)
// LLM value types (Citation, Usage, etc.) have been migrated; remaining violations need
// broader refactoring to introduce domain abstractions for DB traits.
#![allow(unknown_lints)]
#![allow(de0301_no_infra_in_domain)]

pub mod citation_mapping;
pub mod error;
pub mod llm;
pub mod mime_validation;
pub mod model;
pub mod models;
pub mod ports;
pub mod repos;
pub mod retrieval;
pub mod service;
pub mod stream_events;
