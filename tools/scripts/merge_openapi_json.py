#!/usr/bin/env python3
import json
import sys
from pathlib import Path


def load_json(path: Path) -> dict:
    if not path.exists():
        return {}
    with path.open("r", encoding="utf-8") as f:
        return json.load(f)


def merge_dict(base: dict, incoming: dict, key: str) -> None:
    base_section = base.setdefault(key, {})
    incoming_section = incoming.get(key, {})
    for name, value in incoming_section.items():
        base_section[name] = value


def main() -> None:
    if len(sys.argv) != 3:
        print("Usage: merge_openapi_json.py <base-openapi.json> <incoming-openapi.json>", file=sys.stderr)
        sys.exit(1)

    base_path = Path(sys.argv[1])
    incoming_path = Path(sys.argv[2])

    if not incoming_path.exists():
        print(f"Error: incoming OpenAPI file does not exist: {incoming_path}", file=sys.stderr)
        sys.exit(1)

    base = load_json(base_path)
    incoming = load_json(incoming_path)

    if not base:
        base = incoming
    else:
        merge_dict(base, incoming, "paths")
        base_components = base.setdefault("components", {})
        for component_key, incoming_components in incoming.get("components", {}).items():
            target_components = base_components.setdefault(component_key, {})
            for name, value in incoming_components.items():
                target_components[name] = value

    base_path.parent.mkdir(parents=True, exist_ok=True)
    with base_path.open("w", encoding="utf-8") as f:
        json.dump(base, f, indent=2, ensure_ascii=False)
        f.write("\n")

    print(f"Merged {incoming_path} into {base_path}")


if __name__ == "__main__":
    main()
