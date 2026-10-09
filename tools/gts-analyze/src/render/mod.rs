pub mod json;
pub mod md;

/// Location classes in the order they appear in rendered output.
pub const LOC_ORDER: [&str; 5] = ["sdk", "main", "plugin", "doc", "other"];

/// Short bracketed tag shown for a location class.
pub fn loc_icon(loc: &str) -> &'static str {
    match loc {
        "sdk" => "[sdk]",
        "doc" => "[doc]",
        "plugin" => "[plugin]",
        "main" => "[main]",
        _ => "[other]",
    }
}

/// A GTS id is a Type if it ends with `~`, else an Instance.
pub fn is_type_id(gts_id: &str) -> bool {
    gts_id.ends_with('~')
}

/// For an instance id `gts.A.B.C.D.v1~x.y._.z.v1`, return the base type `gts.A.B.C.D.v1~`.
/// Returns None when there is no `~` in the id.
pub fn type_prefix(gts_id: &str) -> Option<String> {
    let pos = gts_id.find('~')?;
    Some(format!("{}~", &gts_id[..pos]))
}
