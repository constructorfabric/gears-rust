//! The axes cells are built from, the cells of each scenario, and the
//! `--cells` filter.

use event_broker_sdk::backend::Flush;

use crate::args::ScenarioId;

/// Events per append, and the matching read batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// One event per append, one per read.
    Single,
    /// 100 events per append, up to 1000 per read.
    Batch,
}

impl Size {
    /// Every size, in matrix order.
    pub const ALL: [Self; 2] = [Self::Single, Self::Batch];

    /// Events each append carries.
    pub const fn events_per_append(self) -> usize {
        match self {
            Self::Single => 1,
            Self::Batch => 100,
        }
    }

    /// Most events one read call returns.
    pub const fn max_events(self) -> usize {
        match self {
            Self::Single => 1,
            Self::Batch => 1000,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Batch => "batch",
        }
    }
}

/// How many writers, or how many consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fanout {
    /// A single writer or consumer.
    One,
    /// Sixteen, one per partition.
    Sixteen,
}

impl Fanout {
    /// Every fanout, in matrix order.
    pub const ALL: [Self; 2] = [Self::One, Self::Sixteen];

    /// How many writers or consumers.
    pub const fn count(self) -> usize {
        match self {
            Self::One => 1,
            Self::Sixteen => 16,
        }
    }
}

/// Whether an append waits for its events to be readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Push {
    /// Appends return once their events are readable.
    Sync,
    /// Appends may return before their events are readable.
    Async,
}

impl Push {
    /// Every push mode, in matrix order.
    pub const ALL: [Self; 2] = [Self::Sync, Self::Async];

    /// The append flush this push mode asks for.
    pub const fn flush(self) -> Flush {
        match self {
            Self::Sync => Flush::Sync,
            Self::Async => Flush::Async,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Async => "async",
        }
    }
}

/// How a consumer learns there is something to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    /// Polls `read(Wait::No)`, backing off briefly when nothing came back.
    Sync,
    /// Follows `watch()` heads and reads up to each one.
    Async,
}

impl ReadMode {
    /// Every read mode, in matrix order.
    pub const ALL: [Self; 2] = [Self::Sync, Self::Async];

    const fn label(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Async => "async",
        }
    }
}

/// A B1 cell.
#[derive(Debug, Clone, Copy)]
pub struct IngestCell {
    /// Events per append.
    pub size: Size,
    /// How many writers.
    pub writers: Fanout,
    /// Whether appends wait for their events to be readable.
    pub push: Push,
}

impl IngestCell {
    /// Every B1 cell, in matrix order.
    pub fn all() -> Vec<Self> {
        let mut cells = Vec::new();
        for size in Size::ALL {
            for writers in Fanout::ALL {
                for push in Push::ALL {
                    cells.push(Self {
                        size,
                        writers,
                        push,
                    });
                }
            }
        }
        cells
    }

    /// The cell's name in reports and `--cells` patterns.
    pub fn name(self) -> String {
        format!(
            "{}-{}p-push:{}",
            self.size.label(),
            self.writers.count(),
            self.push.label()
        )
    }
}

/// A B2 cell; B7 runs one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineCell {
    /// Events per append and per read.
    pub size: Size,
    /// Writers and consumers both: the matrix pairs 1p with 1c and 16p with 16c.
    pub fanout: Fanout,
    /// Whether appends wait for their events to be readable.
    pub push: Push,
    /// How consumers learn there is something to read.
    pub read: ReadMode,
}

impl PipelineCell {
    /// The cell B7 soaks.
    pub const SOAK: Self = Self {
        size: Size::Batch,
        fanout: Fanout::Sixteen,
        push: Push::Async,
        read: ReadMode::Async,
    };

    /// Every B2 cell, in matrix order.
    pub fn all() -> Vec<Self> {
        let mut cells = Vec::new();
        for size in Size::ALL {
            for fanout in Fanout::ALL {
                for push in Push::ALL {
                    for read in ReadMode::ALL {
                        cells.push(Self {
                            size,
                            fanout,
                            push,
                            read,
                        });
                    }
                }
            }
        }
        cells
    }

    /// The cell's name in reports and `--cells` patterns.
    pub fn name(self) -> String {
        let n = self.fanout.count();
        format!(
            "{}-{n}p{n}c-push:{}-read:{}",
            self.size.label(),
            self.push.label(),
            self.read.label()
        )
    }
}

/// A B3 cell.
#[derive(Debug, Clone, Copy)]
pub struct CatchupCell {
    /// How many consumers drain the topic.
    pub consumers: Fanout,
    /// Read batch size.
    pub size: Size,
}

impl CatchupCell {
    /// Every B3 cell, in matrix order.
    pub fn all() -> Vec<Self> {
        let mut cells = Vec::new();
        for consumers in Fanout::ALL {
            for size in Size::ALL {
                cells.push(Self { consumers, size });
            }
        }
        cells
    }

    /// The cell's name in reports and `--cells` patterns.
    pub fn name(self) -> String {
        format!(
            "{}c-read:{}",
            self.consumers.count(),
            self.size.max_events()
        )
    }
}

/// Which cells run: `<scenario>/<cell>` names matched against glob patterns.
#[derive(Debug, Clone)]
pub struct CellFilter {
    patterns: Vec<String>,
}

impl CellFilter {
    /// Admits the cells matching any of `patterns`.
    pub fn new(patterns: Vec<String>) -> Self {
        Self { patterns }
    }

    /// Whether `cell` of `scenario` runs.
    pub fn admits(&self, scenario: ScenarioId, cell: &str) -> bool {
        let name = qualified(scenario, cell);
        self.patterns.iter().any(|pattern| glob(pattern, &name))
    }
}

/// The `<scenario>/<cell>` name a cell is filtered and remembered by.
pub fn qualified(scenario: ScenarioId, cell: &str) -> String {
    format!("{scenario}/{cell}")
}

/// `*` matches any run of characters; everything else matches itself.
fn glob(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return name.is_empty();
    };
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let tail: Vec<&str> = parts.collect();
    let Some((last, middle)) = tail.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}
