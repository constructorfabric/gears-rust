//! Sea-ORM-backed repository implementations.
//!
//! Each module here exposes a `*Repo` trait and a Sea-ORM impl. Services
//! depend on the trait (object-safe `Arc<dyn …>`) so unit tests can swap in
//! in-memory mocks without touching a database.
//
// @cpt-cf-chat-engine-infra-repo-root:p3

pub mod message_repo;
pub mod plugin_config_repo;
pub mod reaction_repo;
pub mod session_repo;
pub mod session_type_repo;
pub mod variant_repo;

/// Crate-wide `DBProvider` alias parameterised over the chat-engine
/// domain error.
///
/// Modules elsewhere in the workspace alias `DBProvider<DomainError>` the
/// same way — the alias lets repos receive a single `Arc<ChatEngineDb>`
/// handle whose `conn()` and `transaction_with_config(...)` results map
/// cleanly into [`crate::domain::error::ChatEngineError`] via the
/// `From<DbError>` impl on the error enum.
pub type ChatEngineDb = modkit_db::DBProvider<crate::domain::error::ChatEngineError>;
