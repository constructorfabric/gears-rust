#!/usr/bin/env python3
"""Extract inline #[cfg(test)] mod tests { ... } blocks into separate *_tests.rs files.

Usage:
    python3 scripts/extract_tests.py <directory>

For each .rs file in <directory> (recursive) that contains an inline test module,
the script:
  1. Extracts the test body into <stem>_tests.rs
  2. Replaces the inline block with an out-of-line #[path = "..."] reference
  3. Preserves #[cfg_attr(coverage_nightly, coverage(off))] if present
"""

import os
import sys


def find_inline_test_block(lines):
    """Find the first inline #[cfg(test)] mod ... { } block. Returns (start_idx, end_idx) or None."""
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("//"):
            continue
        compact = stripped.replace(" ", "")
        if compact != "#[cfg(test)]":
            continue
        # Look ahead for "mod <name> {"
        for j in range(i + 1, min(i + 6, len(lines))):
            s = lines[j].strip()
            if s.startswith("//") or s == "" or s.startswith("#["):
                continue
            if s.startswith("mod ") and "{" in s:
                # Found inline test module, now find closing brace
                brace_depth = 0
                mod_end = None
                for k in range(i, len(lines)):
                    for ch in lines[k]:
                        if ch == "{":
                            brace_depth += 1
                        elif ch == "}":
                            brace_depth -= 1
                            if brace_depth == 0:
                                mod_end = k
                                break
                    if mod_end is not None:
                        break
                if mod_end is not None:
                    return (i, mod_end)
            break
    return None


def extract_test_body(lines, start, end):
    """Extract the body of a mod block (everything between { and })."""
    body_lines = []
    inside = False
    for i in range(start, end + 1):
        line = lines[i]
        if not inside:
            if "{" in line:
                inside = True
                after = line[line.index("{") + 1 :]
                if after.strip():
                    body_lines.append(after)
            continue
        if i == end:
            before = line[: line.rindex("}")]
            if before.strip():
                body_lines.append(before)
        else:
            body_lines.append(line)

    # Dedent
    min_indent = 999
    for tl in body_lines:
        if tl.strip():
            min_indent = min(min_indent, len(tl) - len(tl.lstrip()))
    if min_indent == 999:
        min_indent = 0
    body_lines = [tl[min_indent:] if len(tl) >= min_indent else tl for tl in body_lines]
    return "\n".join(body_lines).strip() + "\n"


def has_coverage_attr(lines, start, end):
    for i in range(start, min(start + 4, end)):
        if "coverage" in lines[i]:
            return True
    return False


def has_super_import(text):
    for line in text.split("\n"):
        stripped = line.strip()
        if stripped.startswith("use super::"):
            return True
    return False


def process_file(fpath):
    with open(fpath) as f:
        content = f.read()
    lines = content.split("\n")

    block = find_inline_test_block(lines)
    if block is None:
        return False

    start, end = block
    test_body = extract_test_body(lines, start, end)
    coverage = has_coverage_attr(lines, start, end)

    # Build test file content
    if not has_super_import(test_body):
        test_content = "use super::*;\n\n" + test_body
    else:
        test_content = test_body

    # Build replacement
    stem = os.path.basename(fpath).replace(".rs", "")
    test_filename = f"{stem}_tests.rs"
    replacement = ["#[cfg(test)]"]
    if coverage:
        replacement.append("#[cfg_attr(coverage_nightly, coverage(off))]")
    replacement.append(f'#[path = "{test_filename}"]')
    replacement.append(f"mod {stem}_tests;")

    # Write source
    new_lines = lines[:start] + replacement + [""]
    with open(fpath, "w") as f:
        f.write("\n".join(new_lines))

    # Write test file
    test_filepath = os.path.join(os.path.dirname(fpath), test_filename)
    with open(test_filepath, "w") as f:
        f.write(test_content)

    print(f"  {fpath} -> {test_filename}")
    return True


def find_cfg_test_items(fpath):
    """Find #[cfg(test)] on individual items (not modules) — these also trigger DE1101."""
    with open(fpath) as f:
        content = f.read()
    lines = content.split("\n")
    items = []
    for i, line in enumerate(lines):
        stripped = line.strip()
        compact = stripped.replace(" ", "")
        if compact != "#[cfg(test)]":
            continue
        # Check what follows — if it's NOT a mod declaration, it's an item
        for j in range(i + 1, min(i + 6, len(lines))):
            s = lines[j].strip()
            if s.startswith("//") or s == "" or s.startswith("#["):
                continue
            if not s.startswith("mod "):
                items.append((i, s[:60]))
            break
    return items


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <directory>")
        sys.exit(1)

    target_dir = sys.argv[1]
    count = 0

    for root, dirs, files in os.walk(target_dir):
        for fname in sorted(files):
            if not fname.endswith(".rs"):
                continue
            if fname.endswith("_tests.rs") or fname.endswith("_test.rs"):
                continue
            fpath = os.path.join(root, fname)
            if process_file(fpath):
                count += 1

    print(f"\nExtracted {count} inline test modules.")

    # Report remaining #[cfg(test)] on individual items
    print("\nChecking for #[cfg(test)] on individual items (may need manual fix)...")
    warn_count = 0
    for root, dirs, files in os.walk(target_dir):
        for fname in sorted(files):
            if not fname.endswith(".rs") or fname.endswith("_tests.rs") or fname.endswith("_test.rs"):
                continue
            fpath = os.path.join(root, fname)
            items = find_cfg_test_items(fpath)
            for line_num, preview in items:
                print(f"  WARN: {fpath}:{line_num + 1} — #[cfg(test)] on item: {preview}")
                warn_count += 1
    if warn_count == 0:
        print("  None found.")
    else:
        print(f"\n  {warn_count} items with #[cfg(test)] — these may trigger DE1101.")


if __name__ == "__main__":
    main()
