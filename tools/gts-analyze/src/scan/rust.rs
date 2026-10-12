//! syn-based scanner for a single `.rs` file.

use std::collections::HashSet;
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{
    Attribute, Expr, ExprLit, Item, ItemStruct, Lit, LitStr, Macro, Meta, MetaNameValue, Token,
};

use crate::classify::classify_location;
use crate::model::{InstanceDef, Reference, TypeDef};
use crate::scan::looks_like_gts_id;

/// GTS items collected from a single Rust source file.
#[derive(Default)]
pub struct ScanResult {
    /// Type definitions found in the file.
    pub types: Vec<TypeDef>,
    /// Instance declarations found in the file.
    pub instances: Vec<InstanceDef>,
    /// Other GTS-shaped string references found in the file.
    pub references: Vec<Reference>,
}

/// Parse `text` (the contents of `rel`) with syn and collect GTS types / instances / references.
/// `include_tests=false` makes the visitor skip any `Item` annotated with `#[test]`,
/// `#[bench]`, a `*::test` derivative, or a positive `#[cfg(test)]`.
pub fn scan_file(rel: &Path, text: &str, include_tests: bool) -> ScanResult {
    let mut result = ScanResult::default();
    let Ok(syntax) = syn::parse_file(text) else {
        return result;
    };
    let location = classify_location(rel).to_string();
    let rel_str = rel.to_string_lossy().into_owned();
    let mut visitor = Visitor {
        file_rel: rel_str,
        file_text: text,
        location,
        out: &mut result,
        seen_refs: HashSet::new(),
        include_tests,
    };
    visitor.visit_file(&syntax);
    result
}

// --------------------------------------------------------------------------
// Internal visitor
// --------------------------------------------------------------------------

struct Visitor<'a> {
    file_rel: String,
    file_text: &'a str,
    location: String,
    out: &'a mut ScanResult,
    seen_refs: HashSet<(String, usize)>,
    include_tests: bool,
}

impl<'a> Visitor<'a> {
    fn line_text(&self, line: usize) -> String {
        let zero_based = line.saturating_sub(1);
        let text = self.file_text.lines().nth(zero_based).unwrap_or("").trim();
        if text.chars().count() <= 160 {
            text.to_string()
        } else {
            let mut s: String = text.chars().take(157).collect();
            s.push('…');
            s
        }
    }

    fn push_ref(&mut self, gts_id: String, line: usize) {
        if !self.seen_refs.insert((gts_id.clone(), line)) {
            return;
        }
        let context = self.line_text(line);
        self.out.references.push(Reference {
            gts_id,
            file: self.file_rel.clone(),
            line,
            location: self.location.clone(),
            context,
        });
    }

    fn handle_type_schema_attr(&mut self, attr: &Attribute, struct_name: Option<&str>) {
        let mut td = TypeDef {
            gts_id: String::new(),
            file: self.file_rel.clone(),
            line: attr.span().start().line,
            source_kind: "rust_macro",
            location: self.location.clone(),
            struct_name: struct_name.map(|s| s.to_string()),
            base: None,
            dir_path: None,
            properties: None,
            description: None,
        };

        if let Meta::List(list) = &attr.meta {
            let parsed: syn::Result<Punctuated<MetaNameValue, Token![,]>> =
                list.parse_args_with(Punctuated::parse_terminated);
            if let Ok(nvs) = parsed {
                for nv in nvs {
                    let key = nv
                        .path
                        .get_ident()
                        .map(|i| i.to_string())
                        .unwrap_or_default();
                    let val = expr_to_string(&nv.value);
                    match key.as_str() {
                        // gts 0.10.0 renamed the attribute `schema_id` ->
                        // `type_id`; accept either form.
                        "type_id" | "schema_id" => {
                            if let Some(v) = val {
                                td.gts_id = v;
                            }
                        }
                        "base" => td.base = val,
                        "dir_path" => td.dir_path = val,
                        "properties" => td.properties = val,
                        "description" => td.description = val,
                        _ => {}
                    }
                }
            }
        }

        if !td.gts_id.is_empty() {
            self.out.types.push(td);
        }
    }
}

impl<'ast, 'a> Visit<'ast> for Visitor<'a> {
    fn visit_item(&mut self, item: &'ast Item) {
        if !self.include_tests && item_is_test(item) {
            return;
        }
        syn::visit::visit_item(self, item);
    }

    fn visit_item_struct(&mut self, s: &'ast ItemStruct) {
        for attr in &s.attrs {
            if attr.path().is_ident("gts_type_schema") {
                self.handle_type_schema_attr(attr, Some(&s.ident.to_string()));
            }
        }
        syn::visit::visit_item_struct(self, s);
    }

    fn visit_macro(&mut self, m: &'ast Macro) {
        let name = m
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        let line = m.span().start().line;

        match name.as_str() {
            "gts_instance" => {
                let typed_as = first_ident(&m.tokens);
                if let Some(id) = find_id_field(&m.tokens) {
                    self.out.instances.push(InstanceDef {
                        gts_id: id,
                        file: self.file_rel.clone(),
                        line,
                        source_kind: "rust_macro",
                        location: self.location.clone(),
                        typed_as,
                    });
                }
            }
            "gts_instance_raw" => {
                let id = find_id_field(&m.tokens).or_else(|| {
                    let mut lits = Vec::new();
                    collect_str_lits(&m.tokens, &mut lits);
                    lits.into_iter()
                        .find(|(s, _)| looks_like_gts_id(s))
                        .map(|(s, _)| s)
                });
                if let Some(id) = id {
                    self.out.instances.push(InstanceDef {
                        gts_id: id,
                        file: self.file_rel.clone(),
                        line,
                        source_kind: "rust_macro_raw",
                        location: self.location.clone(),
                        typed_as: None,
                    });
                }
            }
            "struct_to_gts_schema" => {
                let mut lits = Vec::new();
                collect_str_lits(&m.tokens, &mut lits);
                if let Some((id, lit_line)) = lits.into_iter().find(|(s, _)| looks_like_gts_id(s)) {
                    self.out.types.push(TypeDef {
                        gts_id: id,
                        file: self.file_rel.clone(),
                        line: lit_line.max(line),
                        source_kind: "struct_to_gts_schema",
                        location: self.location.clone(),
                        struct_name: None,
                        base: None,
                        dir_path: None,
                        properties: None,
                        description: None,
                    });
                }
            }
            _ => {
                let mut lits = Vec::new();
                collect_str_lits(&m.tokens, &mut lits);
                for (s, lit_line) in lits {
                    if looks_like_gts_id(&s) {
                        self.push_ref(s, lit_line);
                    }
                }
            }
        }
        syn::visit::visit_macro(self, m);
    }

    fn visit_attribute(&mut self, a: &'ast Attribute) {
        if let Meta::List(list) = &a.meta {
            let mut lits = Vec::new();
            collect_str_lits(&list.tokens, &mut lits);
            for (s, lit_line) in lits {
                if looks_like_gts_id(&s) {
                    self.push_ref(s, lit_line);
                }
            }
        }
        syn::visit::visit_attribute(self, a);
    }

    fn visit_lit_str(&mut self, lit: &'ast LitStr) {
        let s = lit.value();
        if looks_like_gts_id(&s) {
            let line = lit.span().start().line;
            self.push_ref(s, line);
        }
    }
}

// --------------------------------------------------------------------------
// Test-attribute detection
// --------------------------------------------------------------------------

fn attr_marks_test(attr: &Attribute) -> bool {
    if let Some(last) = attr.path().segments.last() {
        let s = last.ident.to_string();
        if s == "test" || s == "bench" {
            return true;
        }
    }
    if attr.path().is_ident("cfg")
        && let Meta::List(list) = &attr.meta
        && let Ok(inner) = list.parse_args::<Meta>()
    {
        return cfg_meta_is_positive_test(&inner);
    }
    false
}

fn cfg_meta_is_positive_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(p) => p.is_ident("test"),
        Meta::List(list) => {
            let head = list
                .path
                .get_ident()
                .map(|i| i.to_string())
                .unwrap_or_default();
            match head.as_str() {
                "not" => false,
                "any" | "all" => list
                    .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                    .map(|metas| metas.iter().any(cfg_meta_is_positive_test))
                    .unwrap_or(false),
                _ => false,
            }
        }
        _ => false,
    }
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

fn item_is_test(item: &Item) -> bool {
    item_attrs(item).iter().any(attr_marks_test)
}

// --------------------------------------------------------------------------
// Token-tree helpers
// --------------------------------------------------------------------------

fn find_id_field(tokens: &TokenStream) -> Option<String> {
    let mut iter = tokens.clone().into_iter().peekable();
    while let Some(tt) = iter.next() {
        match tt {
            TokenTree::Ident(ref ident) if ident == "id" => {
                if let Some(TokenTree::Punct(p)) = iter.peek()
                    && (p.as_char() == ':' || p.as_char() == '=')
                {
                    iter.next();
                    if let Some(TokenTree::Literal(lit)) = iter.next()
                        && let Some(s) = unquote_str_lit(&lit.to_string())
                    {
                        return Some(s);
                    }
                }
            }
            TokenTree::Group(g) => {
                if let Some(id) = find_id_field(&g.stream()) {
                    return Some(id);
                }
            }
            _ => {}
        }
    }
    None
}

fn first_ident(tokens: &TokenStream) -> Option<String> {
    for tt in tokens.clone() {
        if let TokenTree::Ident(ident) = tt {
            return Some(ident.to_string());
        }
    }
    None
}

fn collect_str_lits(tokens: &TokenStream, out: &mut Vec<(String, usize)>) {
    for tt in tokens.clone() {
        match tt {
            TokenTree::Literal(lit) => {
                if let Some(s) = unquote_str_lit(&lit.to_string()) {
                    out.push((s, lit.span().start().line));
                }
            }
            TokenTree::Group(g) => collect_str_lits(&g.stream(), out),
            _ => {}
        }
    }
}

fn unquote_str_lit(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix("r") {
        let mut chars = rest.chars();
        let mut hashes = 0usize;
        for c in chars.by_ref() {
            if c == '#' {
                hashes += 1;
            } else if c == '"' {
                break;
            } else {
                return None;
            }
        }
        let body: String = chars.collect();
        let suffix = format!("\"{}", "#".repeat(hashes));
        body.strip_suffix(&suffix).map(|s| s.to_string())
    } else if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        let inner = &raw[1..raw.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('0') => out.push('\0'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        Some(out)
    } else {
        None
    }
}

fn expr_to_string(e: &Expr) -> Option<String> {
    match e {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Some(s.value()),
        Expr::Lit(ExprLit {
            lit: Lit::Bool(b), ..
        }) => Some(b.value.to_string()),
        Expr::Lit(ExprLit {
            lit: Lit::Int(i), ..
        }) => Some(i.base10_digits().to_string()),
        Expr::Path(p) => Some(
            p.path
                .segments
                .iter()
                .map(|seg| seg.ident.to_string())
                .collect::<Vec<_>>()
                .join("::"),
        ),
        _ => None,
    }
}
