#!/usr/bin/env python3
"""Copy the contract set into the gateway module so it can be embedded.

`go:embed` cannot reach outside its own module, so the gateway cannot embed
`contracts/` directly. Copying is the price; a check that the copy is current is
what keeps it honest.

    contracts/public.openapi.yaml  →  apps/gateway/internal/assets/openapi/openapi.json
    contracts/events/*.json        →  apps/gateway/internal/assets/events/

The contract is converted to JSON here rather than in Go. The gateway serves
`/openapi.json`, and the alternatives were a YAML parser in the binary or a
conversion at startup — both of which put the conversion in the runtime rather than
in the build, where a failure is a red check instead of a broken endpoint.

The second copy is a build artifact committed to the repository, deliberately: the
gateway image is built from `apps/gateway` with the repository as context, and a
generated-at-build-time copy would mean the binary in production could be built
from a contract nobody reviewed. `--check` runs in CI, so a stale copy fails the
build instead of shipping.

`contracts/events/` is copied whole — including `index.json` — because the
playground lists the event types from it.

Usage:
    python3 tools/gen_gateway_assets.py --write
    python3 tools/gen_gateway_assets.py --check
"""

from __future__ import annotations

import argparse
import filecmp
import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compose_contracts as composer

ASSETS = composer.REPO_ROOT / "apps" / "gateway" / "internal" / "assets"
OPENAPI_TARGET = ASSETS / "openapi" / "openapi.json"
EVENTS_SOURCE = composer.CONTRACTS_DIR / "events"
EVENTS_TARGET = ASSETS / "events"


def contract_json() -> str:
    """The composed contract as JSON, formatted exactly as it will be served.

    Key order follows the YAML, so the served document reads in the same order a
    reviewer read the source. `ensure_ascii=False` keeps the non-ASCII that appears
    in examples — place names with accents — readable in the playground.
    """
    document = composer.load_yaml(composer.PUBLIC_DOCUMENT)
    return json.dumps(document, indent=2, ensure_ascii=False) + "\n"


def expected_copies() -> dict[Path, Path]:
    """source -> destination for every file the gateway embeds."""
    copies = {source: EVENTS_TARGET / source.name for source in sorted(EVENTS_SOURCE.glob("*.json"))}
    return copies


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--write", action="store_true", help="refresh the copies")
    action.add_argument("--check", action="store_true", help="fail if a copy is missing or stale")
    args = parser.parse_args()

    copies = expected_copies()
    if not copies:
        print("  error  contracts/events/ is empty", file=sys.stderr)
        return 1

    rendered = contract_json()

    if args.write:
        for source, target in copies.items():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
        OPENAPI_TARGET.parent.mkdir(parents=True, exist_ok=True)
        OPENAPI_TARGET.write_text(rendered, encoding="utf-8")
        print(f"wrote {len(copies) + 1} file(s) into {ASSETS.relative_to(composer.REPO_ROOT)}")
        return 0

    problems: list[str] = []
    openapi_shown = OPENAPI_TARGET.relative_to(composer.REPO_ROOT)
    if not OPENAPI_TARGET.exists():
        problems.append(f"{openapi_shown} is missing")
    elif OPENAPI_TARGET.read_text(encoding="utf-8") != rendered:
        problems.append(f"{openapi_shown} differs from a fresh conversion of {composer.PUBLIC_DOCUMENT.name}")

    for source, target in copies.items():
        shown = target.relative_to(composer.REPO_ROOT)
        if not target.exists():
            problems.append(f"{shown} is missing")
        elif not filecmp.cmp(source, target, shallow=False):
            problems.append(f"{shown} differs from {source.relative_to(composer.REPO_ROOT)}")

    # A file left behind after its source was removed is worse than a missing one:
    # the binary would serve an event schema for an event that no longer exists.
    if EVENTS_TARGET.exists():
        for present in sorted(EVENTS_TARGET.glob("*.json")):
            if present not in copies.values():
                problems.append(f"{present.relative_to(composer.REPO_ROOT)} has no source in contracts/events/")

    if problems:
        for problem in problems:
            print(f"  error  {problem}", file=sys.stderr)
        print("  hint   run 'python3 tools/gen_gateway_assets.py --write'", file=sys.stderr)
        return 1

    print(f"OK: {len(copies) + 1} embedded file(s) match the contract set")
    return 0


if __name__ == "__main__":
    sys.exit(main())
