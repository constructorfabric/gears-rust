//! `check-test-container-pins` — enforce that every database container in the
//! workspace comes from `libs/test-containers`.
//!
//! # Why a parser and not a grep
//!
//! Image tags are pinned in one place (`libs/test-containers`, docs/TESTING.md
//! 4.4) so a version change is one reviewed diff. A fixture that calls the
//! upstream default constructors instead re-introduces the transitive pin
//! through `Cargo.lock` that issue #4616 removed: a dependency bump then moves
//! the database under every test with nothing in the diff to show it.
//!
//! This started life as two `grep -E` steps in CI. A regex matches text, so it
//! matched only the *spelling* it was given:
//!
//! ```ignore
//! use testcontainers_modules::postgres::Postgres as Pg;
//! Pg::default()                                   // invisible to the regex
//! GenericImage::new("mariadb".to_owned(), tag)     // ditto — not a bare literal
//! ```
//!
//! The old gate papered over the first case by *also* banning `Postgres as _`
//! imports, which is a rule about how you write `use` statements rather than
//! about the policy anyone cares about. Parsing with `syn` addresses the real
//! rule directly: it resolves the local name for each banned type, then looks
//! at calls, so formatting, comments, line breaks and renames stop mattering.
//!
//! # The rules
//!
//! 1. `Postgres::default()` / `Mysql::default()` — the upstream constructors
//!    whose tag comes from `testcontainers-modules`.
//! 2. `GenericImage::new(..)` — any argument, anywhere outside the centralized
//!    crate. Banning the constructor outright is both simpler and safer than
//!    trying to evaluate its first argument; every legitimate database generic
//!    image in this workspace already comes from `test_containers`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, ExitCode};

use syn::spanned::Spanned;
use syn::visit::Visit;

/// Crate that *implements* the pins, and so is allowed to call the constructors.
const IMPL_CRATE_PREFIX: &str = "libs/test-containers/";

/// Upstream constructors taking their tag from `testcontainers-modules`.
const BANNED_DEFAULT_TYPES: [&str; 2] = ["Postgres", "Mysql"];

/// Constructor banned outright outside [`IMPL_CRATE_PREFIX`].
const GENERIC_IMAGE_TYPE: &str = "GenericImage";

const POINTER: &str =
    "Database images must come from libs/test-containers (see docs/TESTING.md 4.4).";

pub fn check(args: &[String]) -> ExitCode {
    if let Some(flag) = args.first() {
        eprintln!("error: unexpected argument `{flag}`");
        eprintln!("usage: cargo xtask check-test-container-pins");
        return ExitCode::FAILURE;
    }

    let files = match tracked_rust_files() {
        Ok(files) => files,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut violations = Vec::new();
    let mut scanned = 0usize;

    for file in &files {
        if file.starts_with(IMPL_CRATE_PREFIX) {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(Path::new(file)) else {
            // A tracked path git knows about but we cannot read (sparse
            // checkout, symlink into nowhere) is not a policy violation.
            eprintln!("warning: cannot read {file}, skipping");
            continue;
        };
        scanned += 1;
        match scan_source(&source) {
            Ok(found) => violations.extend(
                found
                    .into_iter()
                    .map(|v| format!("{file}:{}:{} {}", v.line, v.column, v.rule)),
            ),
            Err(err) => {
                // Deliberately-broken sources are tracked on purpose (trybuild
                // UI fixtures). Failing the build on them would make this gate
                // a syntax checker, which it is not.
                eprintln!("warning: cannot parse {file} ({err}), skipping");
            }
        }
    }

    if violations.is_empty() {
        eprintln!("check-test-container-pins: {scanned} files scanned, no violations");
        return ExitCode::SUCCESS;
    }

    let mut report = String::new();
    for violation in &violations {
        let _ = writeln!(report, "  {violation}");
    }
    eprintln!("::error::{POINTER}");
    eprint!("{report}");
    ExitCode::FAILURE
}

/// Tracked `*.rs` paths, workspace-relative.
///
/// `git ls-files` rather than a directory walk: it excludes `target/` and every
/// other ignored path by construction, and it means the gate covers exactly
/// what a reviewer would see in the diff.
fn tracked_rust_files() -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", "*.rs"])
        .output()
        .map_err(|err| format!("cannot run `git ls-files`: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "`git ls-files` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout =
        String::from_utf8(output.stdout).map_err(|err| format!("non-UTF-8 path: {err}"))?;
    Ok(stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// One offending call.
#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub line: usize,
    pub column: usize,
    pub rule: String,
}

/// Parse `source` and report every banned constructor call in it.
pub fn scan_source(source: &str) -> Result<Vec<Violation>, syn::Error> {
    let file = syn::parse_file(source)?;
    let mut names = Names::default();
    names.collect(&file);

    let mut visitor = PinVisitor {
        names: &names,
        violations: Vec::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.violations)
}

/// The local names each banned type answers to in one file.
///
/// The canonical names are always in the set, so an unimported or
/// fully-qualified call is caught the same as an imported one. On top of that
/// we add every rename and type alias, which is what the old regex could not
/// see.
#[derive(Default)]
struct Names {
    banned_default: HashSet<String>,
    generic_image: HashSet<String>,
}

impl Names {
    fn collect(&mut self, file: &syn::File) {
        for name in BANNED_DEFAULT_TYPES {
            self.banned_default.insert(name.to_owned());
        }
        self.generic_image.insert(GENERIC_IMAGE_TYPE.to_owned());

        self.collect_items(&file.items);
    }

    /// Aliases declared inside a `mod tests { .. }` block are the common case
    /// in this workspace, so inline modules are walked too.
    fn collect_items(&mut self, items: &[syn::Item]) {
        for item in items {
            match item {
                syn::Item::Use(item) => self.collect_use(&item.tree),
                syn::Item::Type(item) => self.collect_type_alias(item),
                syn::Item::Mod(syn::ItemMod {
                    content: Some((_, items)),
                    ..
                }) => self.collect_items(items),
                _ => {}
            }
        }
    }

    /// Walk a `use` tree, recording `X as Y` renames of the banned types.
    ///
    /// Globs need no handling: `use ..::postgres::*;` brings in `Postgres`
    /// under its canonical name, which is already in the set.
    fn collect_use(&mut self, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(path) => self.collect_use(&path.tree),
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    self.collect_use(tree);
                }
            }
            syn::UseTree::Rename(rename) => {
                let original = rename.ident.to_string();
                let local = rename.rename.to_string();
                if BANNED_DEFAULT_TYPES.contains(&original.as_str()) {
                    self.banned_default.insert(local);
                } else if original == GENERIC_IMAGE_TYPE {
                    self.generic_image.insert(local);
                }
            }
            syn::UseTree::Name(_) | syn::UseTree::Glob(_) => {}
        }
    }

    /// `type Pg = Postgres;` — resolved one level deep, which covers the form
    /// anyone would plausibly write. Chained aliases are not followed.
    fn collect_type_alias(&mut self, item: &syn::ItemType) {
        let syn::Type::Path(path) = &*item.ty else {
            return;
        };
        let Some(target) = path.path.segments.last() else {
            return;
        };
        let target = target.ident.to_string();
        let local = item.ident.to_string();
        if self.banned_default.contains(&target) {
            self.banned_default.insert(local);
        } else if self.generic_image.contains(&target) {
            self.generic_image.insert(local);
        }
    }
}

struct PinVisitor<'a> {
    names: &'a Names,
    violations: Vec<Violation>,
}

impl PinVisitor<'_> {
    /// `Type::method` in a call position, by the last two path segments — so
    /// `Pg::default()` and `testcontainers_modules::postgres::Postgres::default()`
    /// are the same match.
    fn banned_rule(&self, path: &syn::Path) -> Option<String> {
        let mut segments = path.segments.iter().rev();
        let method = segments.next()?.ident.to_string();
        let ty = segments.next()?.ident.to_string();

        match method.as_str() {
            "default" if self.names.banned_default.contains(&ty) => Some(format!(
                "`{ty}::default()` takes its tag from testcontainers-modules; \
                 use test_containers::postgres()/mysql() instead"
            )),
            "new" if self.names.generic_image.contains(&ty) => Some(format!(
                "`{ty}::new(..)` builds an unpinned image; \
                 use test_containers::timescaledb()/mariadb() instead"
            )),
            _ => None,
        }
    }

    fn report(&mut self, span: proc_macro2::Span, rule: String) {
        let start = span.start();
        self.violations.push(Violation {
            line: start.line,
            column: start.column + 1,
            rule,
        });
    }
}

/// Peel `(expr)` and the invisible grouping macros introduce, so a wrapped
/// callee (`(Postgres::default)()`) is seen the same as a bare one.
fn unwrap_expr(mut expr: &syn::Expr) -> &syn::Expr {
    loop {
        expr = match expr {
            syn::Expr::Paren(paren) => &paren.expr,
            syn::Expr::Group(group) => &group.expr,
            _ => return expr,
        };
    }
}

impl<'ast> Visit<'ast> for PinVisitor<'_> {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = unwrap_expr(&call.func)
            && let Some(rule) = self.banned_rule(&path.path)
        {
            self.report(path.span(), rule);
        }
        syn::visit::visit_expr_call(self, call);
    }
}

#[cfg(test)]
#[path = "pins_tests.rs"]
mod tests;
