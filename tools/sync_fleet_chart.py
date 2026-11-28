#!/usr/bin/env python3
"""Copy the Myota chart into every Fleet bundle that renders it.

Why a copy exists at all
------------------------
Fleet reads a chart from the files **inside a bundle directory**. `helm.chart` in a
`fleet.yaml` is resolved relative to that file, and Fleet only ships the files under the
GitRepo's `paths` entry, so a bundle at `deploy/fleet/myota-test/` cannot reach a chart at
`deploy/myota` — the path exists in the clone but not in the bundle.

A packaged chart (`.tgz`) inside the bundle does not work either. Fleet stores it as an
encoded blob rather than as chart files, the agent cannot load it, and Helm installs a
synthesised chart with no manifests: the release reports `deployed` while the namespace
stays empty. That silent success is worse than an error, so this repository ships chart
*directories*.

Consequence: the chart is authored once at `deploy/myota` and copied into each bundle
directory as `chart/`. This script performs the copy, and `--check` keeps the copies
honest — the same generated-artifact rule the gateway assets follow.

Secrets are never copied. `deploy/myota/secrets/` holds SOPS files, and a bundle that
carried them would put encrypted credentials into every cluster that reconciles it
(ADR-0003).

Usage
-----
    python3 tools/sync_fleet_chart.py            # write the copies
    python3 tools/sync_fleet_chart.py --check    # fail when a copy has drifted
"""

from __future__ import annotations

import argparse
import hashlib
import shutil
import sys
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
SOURCE_CHART = REPOSITORY_ROOT / "deploy" / "myota"
FLEET_ROOT = REPOSITORY_ROOT / "deploy" / "fleet"

#: Directory names that must never enter a bundle. `secrets` holds SOPS material;
#: The rest is noise that Helm ignores but that would be shipped as bundle resources.
EXCLUDED_DIRECTORY_NAMES = {"secrets", "__pycache__", ".git"}
EXCLUDED_FILE_NAMES = {"README.md"}
EXCLUDED_SUFFIXES = {".tgz", ".pyc", ".swp"}


def chart_files(source_chart: Path) -> dict[str, Path]:
    """Return every chart file that belongs in a bundle, keyed by path relative to it.

    The dict is the whole contract between source and copy: whatever is here gets copied,
    whatever is not here is removed from the copy.
    """
    files: dict[str, Path] = {}
    for path in sorted(source_chart.rglob("*")):
        if not path.is_file():
            continue
        relative = path.relative_to(source_chart)
        if any(part in EXCLUDED_DIRECTORY_NAMES for part in relative.parts[:-1]):
            continue
        if path.name in EXCLUDED_FILE_NAMES or path.suffix in EXCLUDED_SUFFIXES:
            continue
        files[str(relative)] = path
    return files


def bundle_directories() -> list[Path]:
    """Return the Fleet bundle directories whose fleet.yaml renders a bundled chart.

    A bundle opts in by naming a chart inside its own directory — `chart: ./chart` or
    `chart: .`. Anything else (a remote chart, a chart published to a registry) is left
    alone, because there is nothing to copy.
    """
    bundles: list[Path] = []
    for fleet_yaml in sorted(FLEET_ROOT.glob("*/fleet.yaml")):
        chart_reference = _chart_reference(fleet_yaml)
        if chart_reference is None:
            continue
        if chart_reference in {".", "./"}:
            bundles.append(fleet_yaml.parent)
        else:
            bundles.append(fleet_yaml.parent / chart_reference.lstrip("./"))
    return bundles


def _chart_reference(fleet_yaml: Path) -> str | None:
    """Extract `helm.chart` from a fleet.yaml without requiring a YAML parser.

    The Fleet files in this repository are written by hand and reviewed, so the value sits
    on its own line under `helm:`. A full parse would be more defensive than the input needs
    and would add a dependency to a check that must run on a bare checkout.
    """
    for line in fleet_yaml.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("#"):
            continue
        if stripped.startswith("chart:"):
            return stripped.split(":", 1)[1].strip().strip("'\"")
    return None


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def compare(source_files: dict[str, Path], target_directory: Path) -> tuple[list[str], list[str], list[str]]:
    """Return (missing, different, extra) relative paths between source and copy."""
    missing: list[str] = []
    different: list[str] = []
    extra: list[str] = []

    if not target_directory.is_dir():
        return sorted(source_files), [], []

    for relative, source_path in source_files.items():
        target_path = target_directory / relative
        if not target_path.is_file():
            missing.append(relative)
        elif digest(target_path) != digest(source_path):
            different.append(relative)

    for target_path in sorted(target_directory.rglob("*")):
        if not target_path.is_file():
            continue
        relative = str(target_path.relative_to(target_directory))
        if relative not in source_files:
            extra.append(relative)

    return sorted(missing), sorted(different), sorted(extra)


def write_copy(source_files: dict[str, Path], target_directory: Path) -> None:
    """Make the copy identical to the source: write what changed, delete what is gone."""
    target_directory.mkdir(parents=True, exist_ok=True)
    missing, different, extra = compare(source_files, target_directory)

    for relative in extra:
        (target_directory / relative).unlink()
    for relative in missing + different:
        destination = target_directory / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source_files[relative], destination)

    _remove_empty_directories(target_directory)


def _remove_empty_directories(root: Path) -> None:
    """Delete directories left empty by a removed chart file, deepest first."""
    directories = sorted((path for path in root.rglob("*") if path.is_dir()), key=lambda path: len(path.parts), reverse=True)
    for directory in directories:
        if not any(directory.iterdir()):
            directory.rmdir()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--check",
        action="store_true",
        help="report drift and exit non-zero instead of writing the copies",
    )
    arguments = parser.parse_args()

    if not SOURCE_CHART.is_dir():
        print(f"error: chart source {SOURCE_CHART} does not exist", file=sys.stderr)
        return 2

    source_files = chart_files(SOURCE_CHART)
    if not source_files:
        print(f"error: no chart files found under {SOURCE_CHART}", file=sys.stderr)
        return 2

    bundle_directories_found = bundle_directories()
    if not bundle_directories_found:
        print(f"error: no Fleet bundle under {FLEET_ROOT} names a bundled chart", file=sys.stderr)
        return 2

    drifted = False
    for target_directory in bundle_directories_found:
        relative_target = target_directory.relative_to(REPOSITORY_ROOT)
        missing, different, extra = compare(source_files, target_directory)

        if arguments.check:
            if missing or different or extra:
                drifted = True
                print(f"DRIFT {relative_target}")
                for relative in missing:
                    print(f"  missing   {relative}")
                for relative in different:
                    print(f"  different {relative}")
                for relative in extra:
                    print(f"  extra     {relative}")
            else:
                print(f"ok    {relative_target} ({len(source_files)} files)")
            continue

        write_copy(source_files, target_directory)
        changed = len(missing) + len(different) + len(extra)
        state = f"{changed} file(s) updated" if changed else "already in sync"
        print(f"wrote {relative_target} ({len(source_files)} files, {state})")

    if drifted:
        print("", file=sys.stderr)
        print("The chart copies have drifted from deploy/myota.", file=sys.stderr)
        print("Run: python3 tools/sync_fleet_chart.py", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
