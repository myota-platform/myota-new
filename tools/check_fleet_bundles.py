#!/usr/bin/env python3
"""Check every Fleet bundle against the way this repository deploys.

    python3 tools/check_fleet_bundles.py --check

The bundle is the deploy. A mistake in one does not fail loudly — Fleet reports `Ready`, the
namespace stays empty, and the next chunk builds on a deployment that never happened. Three of
those mistakes were made once already (F6, `deploy/fleet/README.md`), so they are checked here
rather than remembered:

  published chart   `helm.chart` names an OCI reference to the chart authored in `deploy/myota`.
                    A chart vendored inside the bundle is rejected: it is a second copy of the
                    deployment definition that goes stale silently, and a packaged `.tgz` does
                    not deploy at all — Fleet installs an empty release and reports `Ready`.
  one version       the bundle's `helm.version` equals the chart's `version` in
                    `deploy/myota/Chart.yaml`. Two numbers, one truth: publishing a chart without
                    pointing the bundle at it would deploy the previous one forever.
  no targets        targeting lives in the GitRepo. Fleet appends a `fleet.yaml`'s targets to the
                    GitRepo's, and two identical targets produce no BundleDeployment and no error.
  no client secret  the repository is public, so a clone needs no credential and the workspace
                    holds no secret.

Exit status is non-zero on any violation.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import yaml

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
CHART_DIRECTORY = REPOSITORY_ROOT / "deploy" / "myota"
FLEET_DIRECTORY = REPOSITORY_ROOT / "deploy" / "fleet"

#: Where published charts live. One registry and one chart namespace for the project, so a
#: bundle cannot pin a chart this repository does not publish.
CHART_REGISTRY_PREFIX = "oci://ghcr.io/myota-platform/charts/"

failures: list[str] = []


def fail(message: str) -> None:
    failures.append(message)


def chart_identity() -> tuple[str, str]:
    """Return the authored chart's (name, version) from `deploy/myota/Chart.yaml`."""
    chart_manifest = yaml.safe_load((CHART_DIRECTORY / "Chart.yaml").read_text(encoding="utf-8"))
    return str(chart_manifest["name"]), str(chart_manifest["version"])


def fleet_files() -> list[Path]:
    """Every `fleet.yaml` under `deploy/fleet/`, sorted so output is stable."""
    return sorted(FLEET_DIRECTORY.glob("*/fleet.yaml"))


def check_bundle(fleet_file: Path, chart_name: str, chart_version: str) -> None:
    """Check one bundle. `bundle` below is the directory Fleet treats as the bundle."""
    bundle = fleet_file.parent
    label = str(bundle.relative_to(REPOSITORY_ROOT))

    document = yaml.safe_load(fleet_file.read_text(encoding="utf-8")) or {}
    helm = document.get("helm") or {}

    # ── the chart is published, and it is the chart in this repository ──────────
    chart_reference = str(helm.get("chart", ""))
    expected_reference = CHART_REGISTRY_PREFIX + chart_name
    if not chart_reference.startswith("oci://"):
        fail(
            f"{label}: helm.chart is {chart_reference!r}; it must be the published OCI reference "
            f"{expected_reference!r}. A chart inside the bundle is a copy that goes stale, and a "
            f"packaged .tgz installs an empty release"
        )
    elif chart_reference != expected_reference:
        fail(f"{label}: helm.chart is {chart_reference!r}; this repository publishes {expected_reference!r}")

    pinned_version = str(helm.get("version", ""))
    if not pinned_version:
        fail(f"{label}: helm.version is not set; a floating chart version is a deploy nobody chose")
    elif pinned_version != chart_version:
        fail(
            f"{label}: helm.version is {pinned_version!r} but the authored chart is "
            f"{chart_version!r}; publish the chart and point the bundle at the same version"
        )

    # ── nothing vendored ────────────────────────────────────────────────────────
    if (bundle / "chart").exists():
        fail(f"{label}: a chart/ directory is vendored into the bundle; publish it to GHCR instead")
    for packaged_chart in sorted(bundle.glob("*.tgz")):
        fail(f"{label}: {packaged_chart.name} is a packaged chart in the bundle; it does not deploy")

    # ── targeting and credentials belong elsewhere ──────────────────────────────
    if "targets" in document:
        fail(f"{label}: the bundle sets targets; Fleet appends them to the GitRepo's and the result deploys nothing")
    if "clientSecretName" in document:
        fail(f"{label}: the bundle sets clientSecretName; this repository is public")

    # ── values files must exist, since Fleet reads them from the bundle ─────────
    if not document.get("defaultNamespace"):
        fail(f"{label}: defaultNamespace is not set")
    for values_file in helm.get("valuesFiles") or []:
        if not (bundle / values_file).is_file():
            fail(f"{label}: valuesFiles names {values_file!r}, which is not in the bundle")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="validate the bundles (default)")
    parser.parse_args()

    chart_name, chart_version = chart_identity()

    bundles = fleet_files()
    if not bundles:
        print(f"error: no fleet.yaml under {FLEET_DIRECTORY}", file=sys.stderr)
        return 2

    for fleet_file in bundles:
        check_bundle(fleet_file, chart_name, chart_version)

    if failures:
        for message in failures:
            print(f"FAIL {message}", file=sys.stderr)
        return 1

    names = ", ".join(str(bundle.parent.relative_to(FLEET_DIRECTORY)) for bundle in bundles)
    print(
        f"OK: {len(bundles)} bundle(s) ({names}) pin {chart_name} {chart_version} from "
        f"{CHART_REGISTRY_PREFIX}, with no vendored chart and no targets"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
