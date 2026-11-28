#!/usr/bin/env python3
"""Check that every image this repository deploys is pinned.

    python3 tools/check_images.py          # what `make verify` runs

Two different rules, for two different kinds of image.

**Our own images** live under `images:` in a Fleet values file and must be
`ghcr.io/<org>/myota-new-<service>@sha256:<64 hex>`. The tag is not a pin: `:main` moves
with every merge, so a Deployment referencing it can change without a commit, and "what is
running in test" becomes a question nobody can answer from the repository. CI writes these
lines (`.github/workflows/_image.yml`); this check is what stops a hand edit from putting a
tag back.

**Third-party images** — PostgreSQL, PostGIS, pgBouncer, nginx — are referenced in the
chart's values and must carry a version tag or a digest. `image: postgres` with no tag is
the same defect one layer out: it silently changes when upstream pushes.

What this deliberately does not require: a pin for a service that has never been published.
A first merge publishes and pins; before that there is no digest to record. The check
reports those as `unpublished` so the gap is visible without failing the build.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

import yaml

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
CHART_VALUES = REPOSITORY_ROOT / "deploy" / "myota" / "values.yaml"
FLEET_VALUES_DIRECTORY = REPOSITORY_ROOT / "deploy" / "fleet"

#: `ghcr.io/<org>/myota-new-<service>@sha256:<64 hex>`
PINNED_IMAGE_PATTERN = re.compile(
    r"^ghcr\.io/(?P<org>[a-z0-9][a-z0-9-]*)/myota-new-(?P<service>[a-z0-9-]+)@sha256:[0-9a-f]{64}$"
)

#: A reference with no tag and no digest — `postgres`, `nginx`.
UNTAGGED_PATTERN = re.compile(r"^[a-z0-9.-]+(?::[0-9]+)?/[a-z0-9._/-]+$|^[a-z0-9._-]+$")

failures: list[str] = []


def fail(message: str) -> None:
    failures.append(message)


def service_directories() -> dict[str, Path]:
    """Services that have a Dockerfile, keyed by the name their image uses.

    `apps/<name>` publishes `myota-new-<name>`; `web/<name>` publishes
    `myota-new-<name>-web`, which is the convention `_web.yml`'s callers use and what the
    pinned digest in the values file says. The mapping is explicit rather than implied by
    the directory name, because `web/admin` publishing `myota-new-admin` would be a second
    name for one thing — and the check below would then report a pin for a service that
    does not exist.
    """
    services: dict[str, Path] = {}
    for dockerfile in sorted((REPOSITORY_ROOT / "apps").glob("*/Dockerfile")):
        services[dockerfile.parent.name] = dockerfile
    for dockerfile in sorted((REPOSITORY_ROOT / "web").glob("*/Dockerfile")):
        services[f"{dockerfile.parent.name}-web"] = dockerfile
    return services


def fleet_values_files() -> list[Path]:
    return sorted(FLEET_VALUES_DIRECTORY.glob("*/values.yaml"))


def check_our_pins(path: Path) -> set[str]:
    """Validate the `images:` block of one values file, returning the services pinned."""
    document = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    images = document.get("images") or {}
    label = str(path.relative_to(REPOSITORY_ROOT))

    if not isinstance(images, dict):
        fail(f"{label}: `images` is not a mapping")
        return set()

    for service, reference in images.items():
        if not isinstance(reference, str):
            fail(f"{label}: images.{service} is not a string")
            continue
        match = PINNED_IMAGE_PATTERN.match(reference)
        if not match:
            hint = " (a tag is not a pin)" if "@sha256:" not in reference else " (wrong registry or name)"
            fail(f"{label}: images.{service} = {reference!r} is not a digest reference{hint}")
            continue
        if match.group("service") != service:
            fail(
                f"{label}: images.{service} points at myota-new-{match.group('service')}; "
                f"the key must match the image it names"
            )

    return set(images)


def check_third_party_tags() -> None:
    """Every image scalar in the chart's values must be tag- or digest-pinned."""
    values = yaml.safe_load(CHART_VALUES.read_text(encoding="utf-8")) or {}

    def walk(node: object, path: str) -> None:
        if isinstance(node, dict):
            for key, child in node.items():
                walk(child, f"{path}.{key}" if path else str(key))
        elif isinstance(node, str) and path.endswith("image"):
            if "@sha256:" in node:
                return
            tail = node.rsplit("/", 1)[-1]
            if ":" not in tail:
                fail(f"deploy/myota/values.yaml: {path} = {node!r} has no tag or digest")

    walk(values, "")


def check_no_floating_tags() -> None:
    """`latest` is never a deliberate choice here, so it is never spelled correctly."""
    for path in sorted((REPOSITORY_ROOT / "deploy").rglob("*")):
        if not path.is_file() or path.suffix not in {".yaml", ".yml", ".tpl"}:
            continue
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
            if re.search(r":latest\b", line) or re.search(r"\blatest\b\s*#", line):
                active = line.split("#", 1)[0]
                if ":latest" in active:
                    fail(f"{path.relative_to(REPOSITORY_ROOT)}:{number}: a floating `:latest` tag")


def main() -> int:
    services = service_directories()
    pinned: set[str] = set()

    for path in fleet_values_files():
        pinned |= check_our_pins(path)

    check_third_party_tags()
    check_no_floating_tags()

    for service in sorted(pinned - set(services)):
        fail(f"images.{service} is pinned but no Dockerfile exists at apps/{service} or web/{service}")

    unpinned = sorted(set(services) - pinned)
    if unpinned:
        # Not a failure: a service publishes on its first merge to main, and until then there
        # is no digest to record. Printed so the gap is visible rather than assumed closed.
        print(f"    unpublished: {', '.join(unpinned)} (no digest yet; CI pins on first merge)")

    if failures:
        print("", file=sys.stderr)
        for message in failures:
            print(f"FAIL {message}", file=sys.stderr)
        print("", file=sys.stderr)
        print("Our images are pinned by .github/workflows/_image.yml: never edit a digest by hand,", file=sys.stderr)
        print("and never replace one with a tag.", file=sys.stderr)
        return 1

    print(
        f"OK: {len(pinned)} image(s) pinned by digest across {len(fleet_values_files())} "
        f"Fleet values file(s); every third-party image is tagged; no floating `latest`"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
