#!/usr/bin/env python3
"""Grouped Makefile help generator.

This script scans a Makefile and prints targets grouped by section
headers of the form `# -------- Section Name --------`.
Per-target comments (simple `# ...` lines) become descriptions.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path


def _color_support() -> bool:
    """Return True if it looks like the terminal supports ANSI colors."""

    if not sys.stdout.isatty():
        return False
    if os.environ.get("NO_COLOR") is not None:
        return False
    term = os.environ.get("TERM", "")
    if term.lower() == "dumb":
        return False
    return True


def _is_help_target(name: str) -> bool:
    return bool(name) and not name.startswith((".", "cf-")) and name == name.lower() and not any(
        char.isspace() for char in name
    )


def generate_help(makefile_path: Path, *included_paths: Path) -> None:
    section: str = ""
    section_order: list[str] = []
    entries: dict[str, list[tuple[int, str, str]]] = {}
    # pending description for the next target: (level, text)
    pending: tuple[int, str] | None = None

    use_color = _color_support()

    # Colors (empty strings when color is disabled)
    if use_color:
        color_primary = "\033[1;37m"  # bright white
        color_secondary = "\033[0;90m"  # dim
        color_reset = "\033[0m"
    else:
        color_primary = ""
        color_secondary = ""
        color_reset = ""

    name_field_width = 22

    try:
        lines = []
        for source in dict.fromkeys((makefile_path, *included_paths)):
            lines.extend(source.read_text(encoding="utf-8").splitlines())
            lines.append("")
    except OSError as exc:
        print(f"ERROR: cannot read {makefile_path}: {exc}", file=sys.stderr)
        sys.exit(1)

    for line in lines:
        stripped = line.rstrip("\n")

        # Detect section headers: e.g. "# -------- Code coverage --------"
        if stripped.startswith("# ---"):
            header = stripped.lstrip("# ").strip()
            # remove leading dashes and optional trailing dashes
            while header.startswith("-"):
                header = header.lstrip("- ").strip()
            while header.endswith("-"):
                header = header.rstrip("- ").rstrip()

            section = header
            if section not in entries:
                entries[section] = []
                section_order.append(section)
            pending = None
            continue

        # Primary comments (one '#') describe primary targets
        if stripped.startswith("# ") and not stripped.startswith("## "):
            text = stripped[2:].strip()
            if pending is None:
                pending = (1, text)
            continue

        # Secondary comments ("## ") describe secondary targets
        if stripped.startswith("## "):
            text = stripped[3:].strip()
            if pending is None:
                pending = (2, text)
            continue

        # Target lines: name:
        if pending and ":" in stripped and not stripped.startswith("#"):
            name_part = stripped.split(":", 1)[0]
            name = name_part.strip()
            if not _is_help_target(name):
                pending = None
                continue

            level, desc = pending

            if section not in entries:
                entries[section] = []
                section_order.append(section)

            entries[section].append((level, name, desc))
            pending = None

    # Print grouped help: primaries first, then secondaries, per section
    first_section_printed = False
    for section in section_order or [""]:
        sect_entries = entries.get(section, [])
        if not sect_entries:
            continue

        primaries = [e for e in sect_entries if e[0] == 1]
        secondaries = [e for e in sect_entries if e[0] == 2]

        if not primaries and not secondaries:
            continue

        if not first_section_printed:
            first_section_printed = True
        print()
        if section:
            print(f"{color_primary}{section}:{color_reset}")
        else:
            print(f"{color_primary}Other targets:{color_reset}")

        for _, name, desc in primaries:
            print(
                f" {color_primary}* {name:<{name_field_width}}{color_reset} - "
                f"{color_secondary}{desc}{color_reset}"
            )
        for _, name, desc in secondaries:
            print(
                f"   {color_secondary}{name:<{name_field_width}}{color_reset} - "
                f"{color_secondary}{desc}{color_reset}"
            )

    if first_section_printed:
        note = (
            f"\nNOTE: '{color_primary}*{color_secondary}' marks primary commands in each section; "
            "unmarked entries are secondary commands."
        )
        print(f"{color_secondary}{note}{color_reset}")
        print()


def main(argv: list[str]) -> None:
    makefiles = [Path(name) for name in argv[1:]] or [Path("Makefile")]
    generate_help(*makefiles)


if __name__ == "__main__":
    main(sys.argv)
