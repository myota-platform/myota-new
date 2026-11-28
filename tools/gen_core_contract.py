#!/usr/bin/env python3
"""Generate the contract `core` serves at `/internal/openapi.json`.

    python3 tools/gen_core_contract.py           # write the asset
    python3 tools/gen_core_contract.py --check   # fail when it is stale

## Why an asset rather than the YAML at runtime

`tools/compose_contracts.py --fetch core=<url>` fetches the live document and
compares it with the committed one, and it parses the response as JSON. `core` could
parse YAML at startup and re-serialise it, but that is a YAML dependency in the
service for a document that never changes between builds. Converting it once, here,
keeps the service to `serde_json` and makes the conversion visible in a diff.

## Why the document is not rewritten

The output is the YAML document converted to JSON with keys in their original order,
and the external `$ref: '../shared/components.yaml#…'` values are left alone. The
composer resolves those itself; a service that inlined them would disagree with the
document the composer reads from disk, and `--check` compares the two.

`make checks-contracts` runs `--check`, so a contract edit without regenerating fails
the gate rather than shipping a service that serves last week's document.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import yaml

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
SOURCE = REPOSITORY_ROOT / "contracts" / "core.openapi.yaml"
ASSET = REPOSITORY_ROOT / "apps" / "core" / "assets" / "openapi.json"


def render() -> str:
    """The document as JSON, two-space indented, with a trailing newline."""
    document = yaml.safe_load(SOURCE.read_text(encoding="utf-8"))
    return json.dumps(document, indent=2, ensure_ascii=False, sort_keys=False) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail when the asset is stale")
    arguments = parser.parse_args()

    if not SOURCE.is_file():
        print(f"error: {SOURCE} does not exist", file=sys.stderr)
        return 2

    rendered = render()

    if arguments.check:
        if not ASSET.is_file():
            print(f"STALE {ASSET.relative_to(REPOSITORY_ROOT)} does not exist", file=sys.stderr)
            print("Run: python3 tools/gen_core_contract.py", file=sys.stderr)
            return 1
        if ASSET.read_text(encoding="utf-8") != rendered:
            print(f"STALE {ASSET.relative_to(REPOSITORY_ROOT)} does not match {SOURCE.name}", file=sys.stderr)
            print("Run: python3 tools/gen_core_contract.py", file=sys.stderr)
            return 1
        print(f"ok    {ASSET.relative_to(REPOSITORY_ROOT)} matches {SOURCE.name}")
        return 0

    ASSET.parent.mkdir(parents=True, exist_ok=True)
    ASSET.write_text(rendered, encoding="utf-8")
    operations = sum(
        1
        for path in yaml.safe_load(rendered).get("paths", {}).values()
        for method in path
        if method in {"get", "post", "put", "patch", "delete"}
    )
    print(f"wrote {ASSET.relative_to(REPOSITORY_ROOT)} ({operations} operation(s))")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
