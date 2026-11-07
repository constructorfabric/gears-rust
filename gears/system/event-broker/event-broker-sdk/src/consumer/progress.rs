use crate::consumer::BatchHandlerOutcome;
use crate::error::EventBrokerError;
use crate::gts::StoredPosition;
use crate::sequence::Sequence;

impl BatchHandlerOutcome {
    /// How many events of the delivered batch at `positions` this outcome processed:
    /// all of them for `Success`, the prefix through the offset for `AdvanceThrough`,
    /// and `None` for `Retry`.
    ///
    /// # Errors
    ///
    /// As [`Self::processed_count_through`], for an `AdvanceThrough` offset.
    pub(crate) fn processed_count(
        &self,
        positions: &[StoredPosition],
    ) -> Result<Option<usize>, EventBrokerError> {
        match self {
            Self::Success => Ok(Some(positions.len())),
            Self::AdvanceThrough { offset } => {
                Self::processed_count_through(*offset, positions).map(Some)
            }
            Self::Retry { .. } => Ok(None),
        }
    }

    /// The length of the delivered prefix at `positions` that ends at `offset`.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidConsumerOptions`] when no delivered event sits at
    /// `offset`.
    pub(crate) fn processed_count_through(
        offset: Sequence,
        positions: &[StoredPosition],
    ) -> Result<usize, EventBrokerError> {
        positions
            .iter()
            .position(|at| at.sequence == offset)
            .map(|idx| idx + 1)
            .ok_or_else(|| EventBrokerError::InvalidConsumerOptions {
                detail: format!(
                    "offset {offset} is not present in delivered offsets {:?}",
                    positions.iter().map(|at| at.sequence).collect::<Vec<_>>()
                ),
            })
    }
}
