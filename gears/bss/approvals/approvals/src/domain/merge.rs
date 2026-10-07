//! K-way merge of source pages by `(submitted_at, id)`. No I/O.
//!
//! Every source is asked on every page, after its own key. A key becomes the last unit taken
//! from that source, or stays when none of its units were taken. There is no exhausted state.
//! `has_more` is set when any source had more, or returned a unit this page did not take.

use std::collections::BTreeMap;

use bss_approvals_sdk::{InboxUnit, Order, SortKey};

/// One source's answer for the page being merged. The merge takes the units it selects.
pub struct SourceAnswer {
    /// The configured source name.
    pub source: String,
    /// Units the source returned, already past its key.
    pub units: Vec<InboxUnit>,
    /// The source has a further unit after `units`.
    pub has_more: bool,
}

/// The merged page and the key each source carries into the next page.
pub struct MergedPage {
    /// The first `limit` units in the asked order.
    pub units: Vec<InboxUnit>,
    /// Each asked source's next key: the last unit taken from it, or the key it arrived with.
    pub keys: BTreeMap<String, Option<SortKey>>,
    /// Another page exists: some source had more, or returned a unit that was not taken.
    pub has_more: bool,
}

/// Merges one round of source answers.
///
/// `incoming` holds the key each source was asked after. A source with no entry starts from
/// `None`. Sources that are not in `pages` are ignored.
#[must_use]
pub fn merge(
    order: Order,
    limit: u32,
    incoming: &BTreeMap<String, Option<SortKey>>,
    mut pages: Vec<SourceAnswer>,
) -> MergedPage {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut taken: Vec<Option<SortKey>> = vec![None; pages.len()];
    let mut units = Vec::new();

    while units.len() < limit {
        let Some((pick, unit)) = take_next(order, &mut pages) else {
            break;
        };
        taken[pick] = Some(SortKey::of(&unit));
        units.push(unit);
    }

    let mut keys = BTreeMap::new();
    let mut has_more = false;
    for (slot, page) in pages.iter().enumerate() {
        let previous = incoming.get(&page.source).copied().flatten();
        let next_key = taken[slot].or(previous);
        keys.insert(page.source.clone(), next_key);
        if page.has_more || !page.units.is_empty() {
            has_more = true;
        }
    }

    MergedPage {
        units,
        keys,
        has_more,
    }
}

/// Moves out the unit that sorts first. The check and the take are the same `drain`, so a later
/// edit cannot index a unit this function did not just see.
fn take_next(order: Order, pages: &mut [SourceAnswer]) -> Option<(usize, InboxUnit)> {
    let pick = next_slot(order, pages)?;
    let unit = pages[pick].units.drain(..1).next()?;
    Some((pick, unit))
}

/// The source whose next unit sorts first, if any source still has one.
fn next_slot(order: Order, pages: &[SourceAnswer]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (slot, page) in pages.iter().enumerate() {
        let Some(unit) = page.units.first() else {
            continue;
        };
        let replace = match best {
            None => true,
            Some(current) => {
                let Some(current_unit) = pages[current].units.first() else {
                    continue;
                };
                comes_first(unit, current_unit, order)
            }
        };
        if replace {
            best = Some(slot);
        }
    }
    best
}

/// Whether `left` sorts before `right` in `order`. Equal keys keep the earlier source.
fn comes_first(left: &InboxUnit, right: &InboxUnit, order: Order) -> bool {
    let left_key = (left.submitted_at, left.id);
    let right_key = (right.submitted_at, right.id);
    match order {
        Order::Asc => left_key < right_key,
        Order::Desc => left_key > right_key,
    }
}
