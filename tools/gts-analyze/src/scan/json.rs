//! JSON file scanner — extracts `$id` from JSON Schema documents and collects
//! GTS-id string-literal references from the JSON body.

use std::path::Path;
use toolkit_gts::GTS_ID_URI_PREFIX;

use crate::classify::classify_location;
use crate::model::{Reference, TypeDef};
use crate::scan::{gts_in_string_re, line_at, shorten_line};

/// Scan a JSON file. Pushes a TypeDef when the document is a GTS schema (`$id` matches),
/// and a Reference for every other GTS string literal it contains.
pub fn scan_file(
    rel: &Path,
    text: &str,
    types: &mut Vec<TypeDef>,
    references: &mut Vec<Reference>,
) {
    let location = classify_location(rel).to_string();
    let rel_str = rel.to_string_lossy().into_owned();

    if let Some(td) = scan_schema(&rel_str, &location, text) {
        types.push(td);
    }
    collect_string_refs(&rel_str, &location, text, references);
}

fn scan_schema(rel: &str, location: &str, text: &str) -> Option<TypeDef> {
    // Cheap pre-check to avoid parsing huge JSON when there's clearly no $id.
    let probably_schema = rel.ends_with(".schema.json") || text.contains("\"$id\"");
    if !probably_schema {
        return None;
    }
    let data: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = data.as_object()?;
    let sid = obj.get("$id")?.as_str()?;
    let sid = sid
        .strip_prefix(GTS_ID_URI_PREFIX)
        .unwrap_or(sid)
        .split('?')
        .next()?
        .split('#')
        .next()?;
    let parsed = gts_id::GtsId::try_new(sid).ok()?;
    if !parsed.is_type() {
        return None;
    }
    let description = obj
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(TypeDef {
        gts_id: sid.to_string(),
        file: rel.to_string(),
        line: 1,
        source_kind: "json_schema",
        location: location.to_string(),
        struct_name: None,
        base: None,
        dir_path: None,
        properties: None,
        description,
    })
}

fn collect_string_refs(rel: &str, location: &str, text: &str, out: &mut Vec<Reference>) {
    for cap in gts_in_string_re().captures_iter(text) {
        let m = cap.get(1).expect("group 1 always present");
        let gts_id = m.as_str().to_string();
        let (line_no, line_text) = line_at(text, m.start());
        out.push(Reference {
            gts_id,
            file: rel.to_string(),
            line: line_no,
            location: location.to_string(),
            context: shorten_line(line_text),
        });
    }
}
