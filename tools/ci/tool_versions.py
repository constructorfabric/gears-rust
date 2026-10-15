#!/usr/bin/env python3
"""Expose the version module to Actions without requiring make or Cargo."""
from __future__ import annotations

import os
from pathlib import Path
import re
import sys

NAMES = {"sccache": "SCCACHE", "cargo-gears": "CARGO_GEARS", "nextest": "NEXTEST",
         "cargo-deny": "DENY", "cargo-hack": "HACK", "cargo-llvm-cov": "LLVM_COV",
         "cargo-shear": "SHEAR"}


def versions() -> dict[str, str]:
    content = (Path(__file__).parent / "make/versions.mk").read_text(encoding="utf-8")
    values = dict(re.findall(r"^(\w+)_VERSION := (\d+\.\d+\.\d+)$", content, re.M))
    return {name: values[key] for name, key in NAMES.items()}


def main() -> None:
    pins = versions()
    tools = os.environ.get("CF_INSTALL_TOOLS", "").replace(",", " ").split()
    unknown = set(tools) - pins.keys()
    if unknown:
        raise SystemExit(f"Unknown CI tools: {', '.join(sorted(unknown))}")
    output = {name.replace("-", "_"): value for name, value in pins.items()}
    output["prebuilt"] = ",".join(f"{tool}@{pins[tool]}" for tool in tools if tool != "cargo-gears")
    output["needs_gears"] = str("cargo-gears" in tools).lower()
    text = "".join(f"{key}={value}\n" for key, value in output.items())
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as stream:
            stream.write(text)
    else:
        sys.stdout.write(text)


if __name__ == "__main__":
    main()
