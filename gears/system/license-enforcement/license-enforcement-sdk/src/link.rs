//! "Exactly one linked" check shared by the admission plugin and the evaluator.

/// The binary does not link exactly one implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LinkError {
    /// No implementation is linked.
    #[error("no implementation is linked into the binary")]
    None,
    /// More than one implementation is linked.
    #[error("{0} implementations are linked into the binary, expected exactly one")]
    Many(usize),
}

/// Returns the single item, or [`LinkError`] if there are zero or several.
///
/// # Errors
///
/// [`LinkError::None`] for zero items, [`LinkError::Many`] for more than one.
pub fn exactly_one<T>(items: impl IntoIterator<Item = T>) -> Result<T, LinkError> {
    let mut items = items.into_iter();
    let first = items.next().ok_or(LinkError::None)?;
    let rest = items.count();
    if rest == 0 {
        Ok(first)
    } else {
        Err(LinkError::Many(rest + 1))
    }
}
