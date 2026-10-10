//! What bounds a queue holds, and how stale the reading behind that bound may be.

use std::time::Duration;

/// Default refresh interval: how long a queue's counter snapshot may lag
/// what other instances have enqueued.
const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

/// Admission limits for one queue.
///
/// A queue with no limits configured accepts work without bound. Configuring
/// one bound leaves the other unbounded.
///
/// ```ignore
/// use toolkit_utils::byte_size::MIB;
///
/// .limits(QueueLimits::builder()
///     .max_entities(100_000)
///     .max_bytes(64 * MIB)
///     .build())
/// ```
#[derive(Debug, Clone, Copy)]
pub struct QueueLimits {
    max_entities: i64,
    max_bytes: i64,
    refresh_interval: Duration,
}

impl QueueLimits {
    /// Start building limits for a queue.
    #[must_use]
    pub const fn builder() -> QueueLimitsBuilder {
        QueueLimitsBuilder {
            max_entities: i64::MAX,
            max_bytes: i64::MAX,
            refresh_interval: DEFAULT_REFRESH_INTERVAL,
        }
    }

    /// Maximum entities the queue may hold unprocessed.
    #[must_use]
    pub const fn max_entities(&self) -> i64 {
        self.max_entities
    }

    /// Maximum payload bytes the queue may hold unprocessed.
    #[must_use]
    pub const fn max_bytes(&self) -> i64 {
        self.max_bytes
    }

    /// The bounds this queue actually configured, measured against `pending`.
    ///
    /// An unset bound is absent, so a refusal never reports `i64::MAX`.
    #[must_use]
    pub fn bounds_against(&self, pending: Volume) -> Bounds {
        self.bounds_of(
            Volume {
                entities: self.max_entities,
                bytes: self.max_bytes,
            },
            pending,
        )
    }

    /// The same, against a derived limit such as a partition's share.
    #[must_use]
    pub fn bounds_of(&self, limit: Volume, pending: Volume) -> Bounds {
        Bounds {
            entities: (self.max_entities != i64::MAX).then_some(Bound {
                limit: limit.entities,
                pending: pending.entities,
            }),
            bytes: (self.max_bytes != i64::MAX).then_some(Bound {
                limit: limit.bytes,
                pending: pending.bytes,
            }),
        }
    }

    /// How often the backlog reading is refreshed from the database, which is
    /// also the window in which another instance's submissions are invisible.
    #[must_use]
    pub const fn refresh_interval(&self) -> Duration {
        self.refresh_interval
    }
}

/// A bound and what is currently held against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound {
    /// The configured maximum.
    pub limit: i64,
    /// What the queue or partition holds, as the refusing instance saw it.
    pub pending: i64,
}

impl std::fmt::Display for Bound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.pending, self.limit)
    }
}

/// Which bounds a refusal was measured against.
///
/// A bound that was never configured is absent rather than a sentinel, so a
/// caller is never told that a queue is full at `i64::MAX` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// The entity bound, if one is configured.
    pub entities: Option<Bound>,
    /// The byte bound, if one is configured.
    pub bytes: Option<Bound>,
}

impl std::fmt::Display for Bounds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for (bound, unit) in [(self.entities, "entities"), (self.bytes, "bytes")] {
            if let Some(bound) = bound {
                if !first {
                    f.write_str(", ")?;
                }
                write!(f, "{bound} {unit}")?;
                first = false;
            }
        }
        if first {
            f.write_str("no configured bound")?;
        }
        Ok(())
    }
}

/// Which of a queue's two predicates refused a submission.
///
/// They are different numbers and a caller cannot interpret one as the other:
/// the queue-wide figure is comparable with [`Outbox::depth`](super::Outbox::depth),
/// the per-partition figure is a share of the queue's allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullScope {
    /// The queue-wide bound.
    Queue,
    /// One partition's share of it, which exists so a skewed partition key
    /// cannot consume the whole allowance.
    Partition {
        /// The partition that tripped.
        partition: u32,
    },
}

impl std::fmt::Display for FullScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queue => f.write_str("the queue holds"),
            Self::Partition { partition } => write!(f, "partition {partition} holds"),
        }
    }
}

/// Builds [`QueueLimits`]. Both bounds default to unbounded.
#[derive(Debug, Clone, Copy)]
pub struct QueueLimitsBuilder {
    max_entities: i64,
    max_bytes: i64,
    refresh_interval: Duration,
}

impl QueueLimitsBuilder {
    /// Bound the number of unprocessed entities.
    #[must_use]
    pub const fn max_entities(mut self, max_entities: i64) -> Self {
        self.max_entities = max_entities;
        self
    }

    /// Bound the unprocessed payload bytes.
    #[must_use]
    pub const fn max_bytes(mut self, max_bytes: i64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Override how often the backlog reading is refreshed.
    ///
    /// Shorter narrows the window in which another instance's submissions are
    /// invisible, and so narrows how far the backlog can overshoot the bound.
    #[must_use]
    pub const fn refresh_interval(mut self, refresh_interval: Duration) -> Self {
        self.refresh_interval = refresh_interval;
        self
    }

    /// Finish.
    #[must_use]
    pub const fn build(self) -> QueueLimits {
        QueueLimits {
            max_entities: self.max_entities,
            max_bytes: self.max_bytes,
            refresh_interval: self.refresh_interval,
        }
    }
}

/// A quantity of outbox work: a number of entities and the payload bytes they
/// carry.
///
/// Every outbox counter moves by a `Volume` and every reading is one, so the
/// admitted, released and reclaimed totals, the difference between any two of
/// them, and a queue's configured bound are all the same shape.
///
/// Signed because that is what the columns are, and because a negative value
/// is legitimate in two places: an unpublished journal delta goes negative
/// when a rolled-back transaction hands its arrival back, and a channel's
/// depth goes negative for one audit interval if a counter was written twice.
/// Both are transient and both are corrected. What a negative value must never
/// be is *hidden*, so the arithmetic saturates at the type's bounds rather
/// than clamping at zero: a subtraction that goes negative stays negative and
/// stays visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Volume {
    /// A number of entities.
    pub entities: i64,
    /// The payload bytes those entities carry.
    pub bytes: i64,
}

impl Volume {
    /// Nothing.
    pub const ZERO: Self = Self {
        entities: 0,
        bytes: 0,
    };

    /// What one payload contributes, wherever it is counted.
    ///
    /// Every counter in the outbox moves by this, on the admitting side and on
    /// the releasing side alike, so the two cannot drift on a definition. The
    /// `bytes` column on the incoming and outgoing rows records the same
    /// number for the vacuum, which has no payload in hand when it reclaims.
    #[must_use]
    pub fn of_payload(payload: &[u8]) -> Self {
        Self {
            entities: 1,
            bytes: i64::try_from(payload.len()).unwrap_or(i64::MAX),
        }
    }

    /// Whether this is no work at all.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.entities == 0 && self.bytes == 0
    }
}

impl std::ops::Add for Volume {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            entities: self.entities.saturating_add(rhs.entities),
            bytes: self.bytes.saturating_add(rhs.bytes),
        }
    }
}

impl std::ops::Sub for Volume {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self {
            entities: self.entities.saturating_sub(rhs.entities),
            bytes: self.bytes.saturating_sub(rhs.bytes),
        }
    }
}
