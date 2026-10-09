//! Markdown report writer. Mirrors the Python `render_md` line-for-line where possible.

use std::collections::BTreeMap;

use crate::model::{InstanceDef, Reference, Report, TypeDef};
use crate::render::{LOC_ORDER, is_type_id, loc_icon, type_prefix};

const MAX_PER_LOC_VERBOSE: usize = 5;

/// Renders the report as Markdown.
pub fn render(rep: &Report) -> String {
    let mut lines: Vec<String> = Vec::new();
    let push = |lines: &mut Vec<String>, s: String| lines.push(s);

    push(&mut lines, "# GTS Analysis Report".to_string());
    push(&mut lines, String::new());
    push(&mut lines, format!("**Gear:** `{}`  ", rep.module_root));
    push(
        &mut lines,
        format!(
            "**Tests included:** `{}`  ",
            if rep.include_tests {
                "yes"
            } else {
                "no (use --include-tests)"
            }
        ),
    );
    push(
        &mut lines,
        format!(
            "**Docs scanned:** `{}`  ",
            if rep.skip_docs {
                "no (--skip-docs)"
            } else {
                "yes (use --skip-docs to omit)"
            }
        ),
    );
    push(
        &mut lines,
        format!(
            "**Verbosity:** `{}`",
            if rep.verbose {
                "verbose"
            } else {
                "compact (use --verbose)"
            }
        ),
    );
    push(&mut lines, String::new());

    // ---- Summary ----
    push(&mut lines, "## Summary".to_string());
    push(&mut lines, String::new());
    push(&mut lines, "| Metric | Count |".to_string());
    push(&mut lines, "| --- | ---: |".to_string());
    let total_files: usize = rep.file_counts.values().sum();
    push(&mut lines, format!("| Files scanned | {total_files} |"));
    for ext in [".rs", ".md", ".json", ".toml", ".yaml", ".yml"] {
        let key = ext.trim_start_matches('.');
        if let Some(&n) = rep.file_counts.get(key)
            && n > 0
        {
            push(&mut lines, format!("|   {ext}  | {n} |"));
        }
    }
    push(
        &mut lines,
        format!("| GTS Types defined here | {} |", rep.types.len()),
    );
    push(
        &mut lines,
        format!("| GTS Instances defined here | {} |", rep.instances.len()),
    );
    push(
        &mut lines,
        format!("| GTS ID references found | {} |", rep.references.len()),
    );
    push(&mut lines, String::new());

    // ---- Group refs by id ----
    let mut refs_by_id: BTreeMap<&str, Vec<&Reference>> = BTreeMap::new();
    for r in &rep.references {
        refs_by_id.entry(&r.gts_id).or_default().push(r);
    }
    let defined_type_ids: std::collections::BTreeSet<&str> =
        rep.types.iter().map(|t| t.gts_id.as_str()).collect();
    let defined_instance_ids: std::collections::BTreeSet<&str> =
        rep.instances.iter().map(|i| i.gts_id.as_str()).collect();

    // ---- GTS Types Defined ----
    push(&mut lines, "## GTS Types Defined Here".to_string());
    push(&mut lines, String::new());
    if rep.types.is_empty() {
        push(&mut lines, "_None._".to_string());
        push(&mut lines, String::new());
    } else {
        let mut by_id: BTreeMap<&str, Vec<&TypeDef>> = BTreeMap::new();
        for t in &rep.types {
            by_id.entry(&t.gts_id).or_default().push(t);
        }
        for (gts_id, defs) in &by_id {
            let kind = if is_type_id(gts_id) {
                "Type"
            } else {
                "Instance-shaped (no trailing `~`)"
            };
            push(&mut lines, format!("### `{gts_id}`"));
            push(&mut lines, String::new());
            push(&mut lines, format!("- **Kind:** {kind}"));
            for d in defs {
                push(
                    &mut lines,
                    format!("- **Schema:** {}", describe_type_def(d)),
                );
            }
            let empty = Vec::new();
            let refs = refs_by_id.get(*gts_id).unwrap_or(&empty);
            push(&mut lines, render_usage(refs, rep.verbose));
            push(&mut lines, String::new());
        }
    }

    // ---- GTS Instances Defined ----
    push(&mut lines, "## GTS Instances Defined Here".to_string());
    push(&mut lines, String::new());
    if rep.instances.is_empty() {
        push(&mut lines, "_None._".to_string());
        push(&mut lines, String::new());
    } else {
        let mut by_id: BTreeMap<&str, Vec<&InstanceDef>> = BTreeMap::new();
        for i in &rep.instances {
            by_id.entry(&i.gts_id).or_default().push(i);
        }
        for (gts_id, defs) in &by_id {
            push(&mut lines, format!("### `{gts_id}`"));
            push(&mut lines, String::new());
            if is_type_id(gts_id) {
                push(
                    &mut lines,
                    "- **Warning:** id ends with `~` — should this be a Type, not an Instance?"
                        .to_string(),
                );
            }
            if let Some(base) = type_prefix(gts_id) {
                push(&mut lines, format!("- **Base type:** `{base}`"));
            }
            for d in defs {
                let kind = if d.source_kind == "rust_macro_raw" {
                    "gts_instance_raw!"
                } else {
                    "gts_instance!"
                };
                let typed = d
                    .typed_as
                    .as_deref()
                    .map(|t| format!(" `{t} {{ ... }}`"))
                    .unwrap_or_default();
                push(
                    &mut lines,
                    format!(
                        "- **Declared:** {kind}{typed} at `{}:{}` ({})",
                        d.file, d.line, d.location
                    ),
                );
            }
            let empty = Vec::new();
            let refs = refs_by_id.get(*gts_id).unwrap_or(&empty);
            push(&mut lines, render_usage(refs, rep.verbose));
            push(&mut lines, String::new());
        }
    }

    // ---- External / Referenced IDs ----
    let mut external: Vec<&str> = refs_by_id
        .keys()
        .filter(|gid| !defined_type_ids.contains(*gid) && !defined_instance_ids.contains(*gid))
        .copied()
        .collect();
    external.sort();

    push(
        &mut lines,
        "## Other GTS IDs Referenced (not defined here)".to_string(),
    );
    push(&mut lines, String::new());
    if external.is_empty() {
        push(&mut lines, "_None._".to_string());
        push(&mut lines, String::new());
    } else {
        push(
            &mut lines,
            "| GTS ID | Kind | sdk | main | plugin | doc | total |".to_string(),
        );
        push(
            &mut lines,
            "| --- | --- | ---: | ---: | ---: | ---: | ---: |".to_string(),
        );
        for gid in &external {
            let refs = &refs_by_id[*gid];
            let cnt = count_by_loc(refs);
            let kind = if is_type_id(gid) { "Type" } else { "Instance" };
            push(
                &mut lines,
                format!(
                    "| `{gid}` | {kind} | {} | {} | {} | {} | {} |",
                    cnt.get("sdk").copied().unwrap_or(0),
                    cnt.get("main").copied().unwrap_or(0),
                    cnt.get("plugin").copied().unwrap_or(0),
                    cnt.get("doc").copied().unwrap_or(0),
                    refs.len(),
                ),
            );
        }
        push(&mut lines, String::new());

        let summary_label = if rep.verbose {
            "Detailed reference list"
        } else {
            "Per-ID file breakdown"
        };
        push(&mut lines, "<details>".to_string());
        push(&mut lines, format!("<summary>{summary_label}</summary>"));
        push(&mut lines, String::new());
        for gid in &external {
            push(&mut lines, format!("#### `{gid}`"));
            push(&mut lines, String::new());
            let refs = &refs_by_id[*gid];
            if rep.verbose {
                let mut sorted: Vec<&&Reference> = refs.iter().collect();
                sorted.sort_by(|a, b| {
                    (a.location.as_str(), a.file.as_str(), a.line).cmp(&(
                        b.location.as_str(),
                        b.file.as_str(),
                        b.line,
                    ))
                });
                for r in sorted {
                    push(
                        &mut lines,
                        format!(
                            "- {} `{}:{}` — `{}`",
                            loc_icon(&r.location),
                            r.file,
                            r.line,
                            r.context
                        ),
                    );
                }
            } else {
                let mut per_file: BTreeMap<(&str, &str), usize> = BTreeMap::new();
                for r in refs {
                    *per_file.entry((&r.location, &r.file)).or_insert(0) += 1;
                }
                for ((loc, fpath), cnt) in per_file {
                    push(&mut lines, format!("- {} `{fpath}` ({cnt})", loc_icon(loc)));
                }
            }
            push(&mut lines, String::new());
        }
        push(&mut lines, "</details>".to_string());
        push(&mut lines, String::new());
    }

    // Trim trailing blank lines and append a single newline (matches Python `.rstrip() + "\n"`).
    while lines.last().map(|s| s.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn count_by_loc(refs: &[&Reference]) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for r in refs {
        *out.entry(r.location.clone()).or_insert(0) += 1;
    }
    out
}

fn render_usage(refs: &[&Reference], verbose: bool) -> String {
    if refs.is_empty() {
        return "- **Usage:** declared only — no other references found.".to_string();
    }
    let cnt = count_by_loc(refs);
    let parts: Vec<String> = LOC_ORDER
        .iter()
        .filter_map(|loc| {
            let n = cnt.get(*loc).copied().unwrap_or(0);
            if n == 0 {
                None
            } else {
                Some(format!("{} {n}", loc_icon(loc)))
            }
        })
        .collect();
    let summary = if parts.is_empty() {
        "no references".to_string()
    } else {
        parts.join(", ")
    };
    let mut lines: Vec<String> = vec![format!("- **Usage:** {summary} (total {})", refs.len())];

    let mut by_loc: BTreeMap<&str, Vec<&Reference>> = BTreeMap::new();
    for r in refs {
        by_loc.entry(r.location.as_str()).or_default().push(r);
    }

    if verbose {
        for loc in LOC_ORDER {
            let Some(items) = by_loc.get(loc) else {
                continue;
            };
            let mut items = items.clone();
            items.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
            lines.push(format!(
                "  - {} {} reference(s):",
                loc_icon(loc),
                items.len()
            ));
            for r in items.iter().take(MAX_PER_LOC_VERBOSE) {
                lines.push(format!("    - `{}:{}` — `{}`", r.file, r.line, r.context));
            }
            if items.len() > MAX_PER_LOC_VERBOSE {
                lines.push(format!(
                    "    - … {} more",
                    items.len() - MAX_PER_LOC_VERBOSE
                ));
            }
        }
    } else {
        for loc in LOC_ORDER {
            let Some(items) = by_loc.get(loc) else {
                continue;
            };
            let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
            for r in items {
                *per_file.entry(&r.file).or_insert(0) += 1;
            }
            lines.push(format!("  - {}:", loc_icon(loc)));
            for (fpath, c) in per_file {
                lines.push(format!("    - `{fpath}` ({c})"));
            }
        }
    }

    lines.join("\n")
}

fn describe_type_def(d: &TypeDef) -> String {
    match d.source_kind {
        "rust_macro" => {
            let mut bits: Vec<String> = vec!["Rust `#[gts_type_schema]`".to_string()];
            if let Some(name) = &d.struct_name {
                bits.push(format!("on struct `{name}`"));
            }
            bits.push(format!("at `{}:{}`", d.file, d.line));
            bits.push(format!("({})", d.location));
            let mut extra: Vec<String> = Vec::new();
            if let Some(v) = &d.base {
                extra.push(format!("base=`{v}`"));
            }
            if let Some(v) = &d.dir_path {
                extra.push(format!("dir_path=`{v}`"));
            }
            if let Some(v) = &d.properties {
                let shown = if v.is_empty() { "<empty>" } else { v.as_str() };
                extra.push(format!("properties=`{shown}`"));
            }
            if !extra.is_empty() {
                bits.push(format!("— {}", extra.join(", ")));
            }
            bits.join(" ")
        }
        "json_schema" => format!("JSON Schema file `{}` ({})", d.file, d.location),
        "struct_to_gts_schema" => {
            let sn = d
                .struct_name
                .as_deref()
                .map(|s| format!(" on `{s}`"))
                .unwrap_or_default();
            format!(
                "Rust `struct_to_gts_schema!`{sn} at `{}:{}` ({})",
                d.file, d.line, d.location
            )
        }
        other => format!("{other} at `{}:{}`", d.file, d.line),
    }
}
