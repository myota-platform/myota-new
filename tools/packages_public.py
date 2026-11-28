#!/usr/bin/env python3
"""Report, or make public, every package this organisation publishes.

    python3 tools/packages_public.py            # report only
    python3 tools/packages_public.py --apply    # make the private ones public

Everything this project publishes is public: the repository, the chart, and the
service images. That is not tidiness — Fleet pulls the chart anonymously and a
participant's browser pulls the images, so a private package is a deploy that
cannot start and a page that does not load. The failure is not loud: the push
succeeds, the package is private, and the problem surfaces later as a pull error
in a pod or a blank screen.

Why this is a command rather than a workflow step
-------------------------------------------------
Changing a package's visibility needs an organisation admin. `GITHUB_TOKEN` —
the only credential a workflow has, and the only one AGENTS.md allows it to have —
cannot do it; the API answers 404 rather than 403, which is why the step this
replaces used `continue-on-error: true` and did nothing at all. Rather than store
an admin token in a workflow, the flip is one command a maintainer runs after the
first publish of a new package name.

The token comes from `GH_TOKEN`, or from `gh auth token` when that is not set. It
needs `admin:org` (plus `write:packages`, which a maintainer's token already has).

Exit status is non-zero only on an API error, never because a package is private:
reporting is the default and `--apply` is what changes things, so `make verify`
can call this only if it is ever wired into CI.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

#: GitHub's package types. Container covers the chart (OCI) and the images; the
#: rest are listed so a package of another type cannot appear unnoticed.
PACKAGE_TYPES = ("container", "npm", "maven", "nuget", "rubygems")

API = "https://api.github.com"


def organisation() -> str:
    """The org that owns the packages, from the Git remote rather than from a guess."""
    remote = subprocess.run(
        ["git", "remote", "get-url", "origin"], capture_output=True, text=True, check=True
    ).stdout.strip()
    # https://github.com/OWNER/REPO.git or git@github.com:OWNER/REPO.git
    tail = remote.split("github.com")[-1].lstrip(":/")
    return tail.split("/")[0]


def token() -> str:
    """Return a token, from the environment first so CI can inject one."""
    from_environment = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if from_environment:
        return from_environment
    result = subprocess.run(["gh", "auth", "token"], capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit("error: no GH_TOKEN set and `gh auth token` failed; run `gh auth login`")
    return result.stdout.strip()


def call(method: str, path: str, auth_token: str, body: dict | None = None) -> tuple[int, object]:
    """One API call. Returns (status, decoded body); 404 is returned, not raised."""
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(
        API + path,
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {auth_token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode(errors="replace")


def packages_of_type(org: str, package_type: str, auth_token: str) -> list[dict]:
    """Every package of one type, following pagination."""
    found: list[dict] = []
    page = 1
    while True:
        status, body = call(
            "GET", f"/orgs/{org}/packages?package_type={package_type}&per_page=100&page={page}", auth_token
        )
        if status != 200:
            print(f"  {package_type}: could not list ({status})", file=sys.stderr)
            return found
        if not isinstance(body, list) or not body:
            return found
        found.extend(body)
        if len(body) < 100:
            return found
        page += 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--apply", action="store_true", help="make the private packages public")
    arguments = parser.parse_args()

    org = organisation()
    auth_token = token()

    private: list[tuple[str, str]] = []
    for package_type in PACKAGE_TYPES:
        for package in packages_of_type(org, package_type, auth_token):
            name, visibility = package["name"], package["visibility"]
            print(f"  {package_type:9s} {name:38s} {visibility}")
            if visibility != "public":
                private.append((package_type, name))

    if not private:
        print(f"OK: every {org} package is public")
        return 0

    if not arguments.apply:
        print("")
        print(f"{len(private)} private package(s). Run with --apply to make them public:")
        for package_type, name in private:
            print(f"  python3 tools/packages_public.py --apply")
        return 0

    failures = 0
    for package_type, name in private:
        # A container package name contains a slash ("charts/myota"), which the API
        # expects URL-encoded as %2F.
        encoded = urllib.parse.quote(name, safe="")
        status, body = call(
            "PATCH", f"/orgs/{org}/packages/{package_type}/{encoded}", auth_token, {"visibility": "public"}
        )
        if status in (200, 204):
            print(f"made public: {package_type}/{name}")
        else:
            failures += 1
            hint = " (the token needs admin:org)" if status == 404 else ""
            print(f"FAILED: {package_type}/{name} → {status}{hint}: {str(body)[:200]}", file=sys.stderr)

    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
