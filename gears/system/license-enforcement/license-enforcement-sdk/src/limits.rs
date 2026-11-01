//! Size limits of the management API. A request over a limit fails with
//! `InvalidArgument` and the reason [`TOO_LARGE`](crate::error::reason::TOO_LARGE).

/// Bytes of an idempotency token.
pub const MAX_IDEMPOTENCY_TOKEN_LEN: usize = 128;
/// Bytes of a resource id or a subject id.
pub const MAX_ID_LEN: usize = 256;
/// Bytes of a GTS type id.
pub const MAX_TYPE_ID_LEN: usize = 1024;
/// Grants in one issue request.
pub const MAX_GRANTS_PER_ISSUE: usize = 1000;
/// Bytes of serialized conditions or recipient rules.
pub const MAX_RULES_LEN: usize = 16 * 1024;
/// Bytes of imported evidence.
pub const MAX_EVIDENCE_LEN: usize = 1024 * 1024;
