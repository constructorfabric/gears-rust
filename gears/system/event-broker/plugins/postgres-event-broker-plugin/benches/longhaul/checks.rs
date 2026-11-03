//! What a reader checks as it reads one partition in order (design D6).

use std::collections::HashMap;
use std::sync::Arc;

use event_broker_sdk::backend::Backend;
use event_broker_sdk::models::Event;
use event_broker_sdk::{Position, Sequence};

use crate::tally::Tally;
use crate::topics::{Topic, stamp_of};

/// One reader's position in each partition of a topic, and the last count it
/// read per writer and key.
pub struct InOrder {
    pub reader: &'static str,
    pub topic: Topic,
    pub positions: Vec<i64>,
    last: HashMap<(u32, u32), u64>,
}

impl InOrder {
    pub fn new(reader: &'static str, topic: Topic) -> Self {
        let positions = vec![0; topic.partitions as usize];
        Self {
            reader,
            topic,
            positions,
            last: HashMap::new(),
        }
    }

    /// Checks `events`, read from `partition` after its position, and moves
    /// the position to the last of them. Returns how many sequences were
    /// skipped because retention removed them first.
    pub async fn accept(
        &mut self,
        backend: &dyn Backend,
        tally: &Arc<Tally>,
        partition: u32,
        events: &[Event],
    ) -> u64 {
        let mut skipped = 0;
        let name = self.topic.kind.name();
        for event in events {
            let Some(sequence) = event.sequence.map(Sequence::as_i64) else {
                tally.violation(format!(
                    "{} {name}/{partition}: an event without a sequence",
                    self.reader
                ));
                continue;
            };
            let at = self.positions[partition as usize];
            if sequence <= at {
                tally.violation(format!(
                    "{} {name}/{partition}: sequence {sequence} after {at}",
                    self.reader
                ));
                continue;
            }
            if sequence > at + 1 {
                // A floor that cannot be read now cannot tell a skip from a
                // gap: the read is counted failed and the skip let through.
                let floor = if let Ok(floor) = backend
                    .resolve(&self.topic.at(partition), Position::Earliest)
                    .await
                {
                    floor.as_i64()
                } else {
                    tally
                        .read_errors
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    i64::MAX
                };
                if sequence - 1 <= floor {
                    skipped += u64::try_from(sequence - 1 - at).unwrap_or(0);
                } else {
                    tally.violation(format!(
                        "{} {name}/{partition}: gap ({at}, {sequence}) above the floor {floor}",
                        self.reader
                    ));
                }
            }
            self.positions[partition as usize] = sequence;
            match stamp_of(event) {
                Some(stamp) => {
                    let last = self.last.entry((stamp.w, stamp.k)).or_insert(0);
                    if stamp.n <= *last {
                        tally.violation(format!(
                            "{} {name}/{partition}: writer {} key {} count {} after {} (sequence {sequence})",
                            self.reader, stamp.w, stamp.k, stamp.n, *last
                        ));
                    }
                    *last = stamp.n;
                }
                None => tally.violation(format!(
                    "{} {name}/{partition}: sequence {sequence} carries no stamp",
                    self.reader
                )),
            }
        }
        skipped
    }
}
