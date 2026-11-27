#!/usr/bin/env python3
"""Copy the shared `ops` DDL into both language modules.

SQL is not language-specific, so the DDL is written once in `libs/ops/sql/` and
copied into each module, where `go:embed` and `include_str!` can reach it. A copy is
needed rather than a shared read because both embedding mechanisms are confined to
their own module directory.

    libs/ops/sql/NNN_*.sql  →  libs/ops/go/sql/NNN_*.sql
                            →  libs/ops/rust/sql/NNN_*.sql

`--check` fails when a copy is stale, so the two implementations cannot be applying
different schemas. That is the one duplication in this library that would be silent:
a column added on one side would make the other side's queries fail at run time, in
production, on one service only.

Usage:
    python3 tools/gen_ops_sql.py --write
    python3 tools/gen_ops_sql.py --check
"""

from __future__ import annotations

import argparse
import filecmp
import shutil
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SOURCE = REPO_ROOT / "libs" / "ops" / "sql"
TARGETS = [
    REPO_ROOT / "libs" / "ops" / "go" / "sql",
    REPO_ROOT / "libs" / "ops" / "rust" / "sql",
]


def migrations() -> list[Path]:
    return sorted(SOURCE.glob("*.sql"))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--write", action="store_true", help="refresh both copies")
    action.add_argument("--check", action="store_true", help="fail if a copy is missing or stale")
    args = parser.parse_args()

    files = migrations()
    if not files:
        print(f"  error  {SOURCE.relative_to(REPO_ROOT)} holds no migrations", file=sys.stderr)
        return 1

    if args.write:
        for target in TARGETS:
            target.mkdir(parents=True, exist_ok=True)
            for source in files:
                shutil.copyfile(source, target / source.name)
            # A file left behind after its source was renamed would be applied by one
            # language and not the other — worse than a missing file, because it looks
            # like a migration that exists.
            for present in sorted(target.glob("*.sql")):
                if present.name not in {source.name for source in files}:
                    present.unlink()
                    print(f"  removed stale {present.relative_to(REPO_ROOT)}")
        print(f"copied {len(files)} migration(s) into {len(TARGETS)} module(s)")
        return 0

    problems: list[str] = []
    for target in TARGETS:
        for source in files:
            copy = target / source.name
            shown = copy.relative_to(REPO_ROOT)
            if not copy.exists():
                problems.append(f"{shown} is missing")
            elif not filecmp.cmp(source, copy, shallow=False):
                problems.append(f"{shown} differs from {source.relative_to(REPO_ROOT)}")
        if target.exists():
            for present in sorted(target.glob("*.sql")):
                if present.name not in {source.name for source in files}:
                    problems.append(f"{present.relative_to(REPO_ROOT)} has no source in libs/ops/sql/")

    if problems:
        for problem in problems:
            print(f"  error  {problem}", file=sys.stderr)
        print("  hint   run 'python3 tools/gen_ops_sql.py --write'", file=sys.stderr)
        return 1

    print(f"OK: {len(files)} migration(s) identical in {len(TARGETS)} module(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
