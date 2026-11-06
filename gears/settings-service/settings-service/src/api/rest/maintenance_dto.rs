//! Wire shapes of the maintenance surface.

/// What one sweep request did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct SweepResultDto {
    /// Expired staged secrets released from the store, their rows deleted.
    pub processed: u64,
    /// Whether a full page of expired rows was found and some of it released,
    /// so more may remain: the scheduler calls again until this is `false`,
    /// which it also is when the store cannot release anything.
    pub more: bool,
}

/// What one audit prune request did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct PruneResultDto {
    /// Audit records deleted by this request.
    pub deleted: u64,
    /// Whether the request's budget ran out before a short batch showed the
    /// backlog was gone: the scheduler calls again until this is `false`.
    pub more: bool,
}
