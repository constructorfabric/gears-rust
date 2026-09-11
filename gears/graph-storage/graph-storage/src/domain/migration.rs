//! Payload migrations: the closed set of steps that moves stored data to a
//! candidate schema.
//!
//! Pure logic, like [`super::evolution`] — this module never sees a row. It
//! compiles a caller's steps into a checked plan and applies that plan to one
//! payload in memory. **The same plan is what the store validates and what the
//! store writes**, which is the whole reason it lives here rather than being
//! rendered into SQL: a migration whose validation and whose write are
//! different code can pass one and fail the other, and the caller would have
//! no way to tell.
//!
//! Three steps, not an expression language (ADR-0006, and § 4.3 of the
//! type-update plan behind ADR-0006). Three covered every incompatible edit the Studio domain model
//! produced in three days, and a closed set is what lets every path be a
//! checked literal.

use graph_storage_sdk::models::{MigrationSpec, MigrationStep};
use serde_json::Value;

use crate::domain::error::DomainError;

/// Steps one migration may carry. A bound rather than a guess: a plan longer
/// than this is a model rewrite, and a model rewrite is a new major.
const MAX_STEPS: usize = 50;

/// The prefix every path must have.
///
/// The top level of an element is closed by the base ontology and owned by the
/// gear (DESIGN § 3.1 authoring rule 2: "derived types extend `payload`,
/// nothing else"), so a migration that could reach `/name` or `/node_key` would
/// be editing the envelope, not the producer's document.
const PAYLOAD: &str = "/payload/";

/// A validated step: the tokens are split once, at compile time.
#[derive(Clone, Debug)]
enum Step {
    Rename { from: Vec<String>, to: Vec<String> },
    Default { path: Vec<String>, value: Value },
    Drop { path: Vec<String> },
}

/// A compiled migration for one type.
#[derive(Clone, Debug)]
pub struct Plan {
    type_id: String,
    steps: Vec<Step>,
}

/// Tokens of a payload path, or the reason it is not one.
fn tokens(path: &str, what: &str) -> Result<Vec<String>, DomainError> {
    let Some(rest) = path.strip_prefix(PAYLOAD) else {
        return Err(DomainError::invalid(format!(
            "migration {what} `{path}` must be a payload path (`{PAYLOAD}…`): the element's top \
             level belongs to the gear, not to the producer's document"
        )));
    };
    if rest.is_empty() {
        return Err(DomainError::invalid(format!(
            "migration {what} `{path}` names no property"
        )));
    }
    let mut out = Vec::new();
    for token in rest.split('/') {
        // The same alphabet a declared `index` path must use, and for a
        // related reason: a path that reaches storage has to be a literal
        // nobody can read two ways.
        if token.is_empty()
            || !token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Err(DomainError::invalid(format!(
                "migration {what} `{path}` has a token outside `[A-Za-z0-9_.-]`"
            )));
        }
        out.push(token.to_owned());
    }
    Ok(out)
}

/// Compile and check one caller's migration.
pub fn compile(spec: &MigrationSpec) -> Result<Plan, DomainError> {
    if spec.steps.is_empty() {
        return Err(DomainError::invalid(format!(
            "the migration for `{}` declares no steps",
            spec.type_id
        )));
    }
    if spec.steps.len() > MAX_STEPS {
        return Err(DomainError::invalid(format!(
            "the migration for `{}` declares {} steps; at most {MAX_STEPS} are admitted",
            spec.type_id,
            spec.steps.len()
        )));
    }

    // Two steps touching one path would make the outcome depend on an order
    // the caller never stated. Refused rather than ordered for them.
    let mut touched: Vec<&str> = spec.steps.iter().flat_map(MigrationStep::paths).collect();
    touched.sort_unstable();
    let mut seen: Option<&str> = None;
    for path in touched {
        if seen == Some(path) {
            return Err(DomainError::invalid(format!(
                "the migration for `{}` touches `{path}` in more than one step; the outcome would \
                 depend on an order it does not state",
                spec.type_id
            )));
        }
        seen = Some(path);
    }

    let mut steps = Vec::with_capacity(spec.steps.len());
    for step in &spec.steps {
        steps.push(match step {
            MigrationStep::Rename { from, to } => {
                if from == to {
                    return Err(DomainError::invalid(format!(
                        "the migration for `{}` renames `{from}` to itself",
                        spec.type_id
                    )));
                }
                Step::Rename {
                    from: tokens(from, "rename source")?,
                    to: tokens(to, "rename target")?,
                }
            }
            MigrationStep::Default { path, value } => Step::Default {
                path: tokens(path, "default path")?,
                value: value.clone(),
            },
            MigrationStep::Drop { path } => Step::Drop {
                path: tokens(path, "drop path")?,
            },
        });
    }
    Ok(Plan {
        type_id: spec.type_id.clone(),
        steps,
    })
}

/// Walk to the object that owns the last token, creating nothing.
fn owner<'a, 'p>(
    payload: &'a mut Value,
    path: &'p [String],
) -> Option<(&'a mut serde_json::Map<String, Value>, &'p str)> {
    let (last, parents) = path.split_last()?;
    let mut cursor = payload;
    for token in parents {
        cursor = cursor.as_object_mut()?.get_mut(token)?;
    }
    Some((cursor.as_object_mut()?, last.as_str()))
}

/// Walk to the object that owns the last token, creating intermediate objects.
///
/// Only the write side of `rename` and `default` uses this: a target path may
/// legitimately not exist yet. It refuses to replace a non-object on the way,
/// because turning a scalar into an object would silently destroy it.
fn owner_or_create<'a, 'p>(
    payload: &'a mut Value,
    path: &'p [String],
) -> Option<(&'a mut serde_json::Map<String, Value>, &'p str)> {
    let (last, parents) = path.split_last()?;
    let mut cursor = payload;
    for token in parents {
        let map = cursor.as_object_mut()?;
        let entry = map
            .entry(token.clone())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !entry.is_object() {
            return None;
        }
        cursor = entry;
    }
    Some((cursor.as_object_mut()?, last.as_str()))
}

impl Plan {
    #[must_use]
    pub fn type_id(&self) -> &str {
        &self.type_id
    }

    /// Apply every step to one payload, in the order the caller declared.
    ///
    /// Returns whether anything changed, so a row that the migration does not
    /// touch is not rewritten — a migration over a type where half the rows
    /// already carry the new shape should cost half the writes.
    #[must_use]
    pub fn apply(&self, payload: &mut Value) -> bool {
        let mut changed = false;
        for step in &self.steps {
            match step {
                Step::Rename { from, to } => {
                    // An absent source is a no-op. A migration moves what is
                    // there; inventing a value is what `default` is for, and
                    // the caller can declare both.
                    let Some(taken) = owner(payload, from).and_then(|(map, key)| map.remove(key))
                    else {
                        continue;
                    };
                    if let Some((map, key)) = owner_or_create(payload, to) {
                        map.insert(key.to_owned(), taken);
                        changed = true;
                    }
                }
                Step::Default { path, value } => {
                    let absent = payload
                        .pointer(&format!("/{}", path.join("/")))
                        .is_none_or(Value::is_null);
                    if absent && let Some((map, key)) = owner_or_create(payload, path) {
                        map.insert(key.to_owned(), value.clone());
                        changed = true;
                    }
                }
                Step::Drop { path } => {
                    if let Some((map, key)) = owner(payload, path)
                        && map.remove(key).is_some()
                    {
                        changed = true;
                    }
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(steps: Vec<MigrationStep>) -> MigrationSpec {
        MigrationSpec {
            type_id: "gts.test.gs._.thing.v1~".to_owned(),
            steps,
        }
    }

    fn rename(from: &str, to: &str) -> MigrationStep {
        MigrationStep::Rename {
            from: from.to_owned(),
            to: to.to_owned(),
        }
    }

    #[test]
    fn a_rename_moves_the_value_and_leaves_nothing_behind() {
        let plan = compile(&spec(vec![rename("/payload/priority", "/payload/urgency")]))
            .expect("the plan compiles");
        let mut payload = json!({ "key": "r1", "priority": "high" });
        assert!(plan.apply(&mut payload));
        assert_eq!(payload, json!({ "key": "r1", "urgency": "high" }));
    }

    /// The row that never had the property is not given one: a migration moves
    /// what is there, and inventing a value is a different step.
    #[test]
    fn a_rename_of_an_absent_property_changes_nothing() {
        let plan = compile(&spec(vec![rename("/payload/priority", "/payload/urgency")]))
            .expect("the plan compiles");
        let mut payload = json!({ "key": "r1" });
        assert!(!plan.apply(&mut payload));
        assert_eq!(payload, json!({ "key": "r1" }));
    }

    #[test]
    fn a_default_fills_an_absent_or_null_value_and_never_overwrites() {
        let plan = compile(&spec(vec![MigrationStep::Default {
            path: "/payload/owner".to_owned(),
            value: json!("unassigned"),
        }]))
        .expect("the plan compiles");

        let mut absent = json!({ "key": "r1" });
        assert!(plan.apply(&mut absent));
        assert_eq!(absent["owner"], json!("unassigned"));

        let mut null = json!({ "key": "r2", "owner": serde_json::Value::Null });
        assert!(plan.apply(&mut null));
        assert_eq!(null["owner"], json!("unassigned"));

        let mut present = json!({ "key": "r3", "owner": "ada" });
        assert!(!plan.apply(&mut present));
        assert_eq!(present["owner"], json!("ada"));
    }

    #[test]
    fn a_drop_removes_only_what_is_there() {
        let plan = compile(&spec(vec![MigrationStep::Drop {
            path: "/payload/legacy_flag".to_owned(),
        }]))
        .expect("the plan compiles");
        let mut with = json!({ "key": "r1", "legacy_flag": true });
        assert!(plan.apply(&mut with));
        assert_eq!(with, json!({ "key": "r1" }));
        let mut without = json!({ "key": "r1" });
        assert!(!plan.apply(&mut without));
    }

    #[test]
    fn nested_paths_are_walked_and_created() {
        let plan = compile(&spec(vec![rename(
            "/payload/loc/line",
            "/payload/position/line",
        )]))
        .expect("the plan compiles");
        let mut payload = json!({ "loc": { "line": 10, "col": 3 } });
        assert!(plan.apply(&mut payload));
        assert_eq!(
            payload,
            json!({ "loc": { "col": 3 }, "position": { "line": 10 } })
        );
    }

    #[test]
    fn a_path_outside_the_payload_is_refused() {
        for path in ["/name", "/node_key", "payload/x", "/payload", "/payload/"] {
            let error = compile(&spec(vec![MigrationStep::Drop {
                path: path.to_owned(),
            }]))
            .expect_err("only payload paths are admitted");
            assert!(
                error.to_string().contains(path) || error.to_string().contains("payload"),
                "{error} for {path}"
            );
        }
    }

    #[test]
    fn a_token_outside_the_alphabet_is_refused() {
        let error = compile(&spec(vec![MigrationStep::Drop {
            path: "/payload/a\"b".to_owned(),
        }]))
        .expect_err("a quotable token is refused");
        assert!(error.to_string().contains("alphabet") || error.to_string().contains("outside"));
    }

    /// Two steps on one path make the answer depend on an order the request
    /// never states, so the request is wrong rather than the outcome arbitrary.
    #[test]
    fn two_steps_on_one_path_are_refused() {
        let error = compile(&spec(vec![
            MigrationStep::Drop {
                path: "/payload/owner".to_owned(),
            },
            MigrationStep::Default {
                path: "/payload/owner".to_owned(),
                value: json!("x"),
            },
        ]))
        .expect_err("an ambiguous plan is refused");
        assert!(error.to_string().contains("more than one step"), "{error}");
    }

    #[test]
    fn an_empty_or_oversized_plan_is_refused() {
        assert!(compile(&spec(Vec::new())).is_err());
        let many = (0..51)
            .map(|i| MigrationStep::Drop {
                path: format!("/payload/f{i}"),
            })
            .collect();
        assert!(compile(&spec(many)).is_err());
    }

    /// The deck's third and fourth edits, as one plan: the rename that moves
    /// the data and the default that fills the newly required field.
    #[test]
    fn the_two_edits_that_needed_a_migration_are_one_plan() {
        let plan = compile(&spec(vec![
            rename("/payload/priority", "/payload/urgency"),
            MigrationStep::Default {
                path: "/payload/owner".to_owned(),
                value: json!("unassigned"),
            },
        ]))
        .expect("the plan compiles");
        let mut payload = json!({ "key": "r1", "statement": "s", "priority": "normal" });
        assert!(plan.apply(&mut payload));
        assert_eq!(
            payload,
            json!({ "key": "r1", "statement": "s", "urgency": "normal", "owner": "unassigned" })
        );
    }
}
