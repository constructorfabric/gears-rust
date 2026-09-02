//! Version families: the key that groups every version of one logical entity, and
//! the three non-stored rules a family is asked at admission.
//!
//! A family groups `v1~`, `v1.4~` and `v2~` of the same type because
//! `database.sql`'s rules are asked of the family row, their only serialization
//! point. The split here is between arithmetic and judgement:
//!
//! * `key` derives the family key and the sibling identifiers a rule needs to
//!   look up. Pure string arithmetic over a parsed identifier — no database, no
//!   clock, no state.
//! * `rules` holds kind, minor shape and minor contiguity. Each is an **exact**
//!   lookup through `uq_tr_entity_gts_id` on an identifier `key` derived, never
//!   a scan of the family.
//!
//! Only [`FamilyKey`], [`family_key`], [`FamilyRefusal`] and [`admits_new_member`]
//! leave the directory — the key the storage layer persists, and the question the
//! commit path asks. `sibling_id`, `version_probe` and `VersionProbe` are the rules'
//! own arithmetic, with no caller outside; publishing them would make any change to
//! the probe's shape a breaking release.

mod key;
mod rules;

pub use key::{FamilyKey, family_key, lock_order};
pub use rules::{FamilyRefusal, admits_new_member};
