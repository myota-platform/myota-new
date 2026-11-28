#!/usr/bin/env python3
"""Report whether every package this organisation publishes is public.

    python3 tools/packages_public.py            # report
    python3 tools/packages_public.py --check    # exit non-zero if anything is private

Everything this project publishes is public: the repository, the chart, the service
images. That is not tidiness — Fleet pulls the chart anonymously and a participant's
browser pulls the images, so a private package is a deploy that cannot start and a page
that does not load.

How a package becomes public (measured, not assumed)
----------------------------------------------------
GitHub packages no longer have a visibility endpoint:

  * REST exposes only `GET`, `DELETE`, `restore` and `versions` for a package — there is no
    `PATCH`. Asking for one returns `404 Not Found`, which reads like a permissions problem
    and is not one. (Verified against the published OpenAPI description and against a
    package this account owns.)
  * GraphQL exposes `deletePackageVersion` and nothing that changes visibility.
  * `GITHUB_TOKEN` cannot read an organisation's packages at all: it gets `404 Package not
    found` for both the read and the write.

What *does* decide visibility is **who published the package**:

  * published by a **workflow** with `GITHUB_TOKEN` from a public repository → **public**,
    and linked to that repository (`repository: myota-platform/myota-new`);
  * published from a laptop with a **personal token** → **private, unlinked**, and no API
    can change it afterwards.

So the mechanism is "never push a package by hand". The remedy for a package that was
pushed by hand is to delete it and let the workflow recreate it — which is why this script
reports rather than flips, and why `--check` is the form CI would use if `GITHUB_TOKEN`
could read packages at all.

Exit status: 0 when everything is public (or, without `--check`, always 0 after a successful
report); 1 when `--check` finds a private package; 2 on an API error.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

PACKAGE_TYPES = ("container", "npm", "maven", "nuget", "rubygems")
API = "https://api.github.com"


def organisation() -> str:
    """The org that owns the packages, read from the Git remote rather than guessed."""
    remote = subprocess.run(
        ["git", "remote", "get-url", "origin"], capture_output=True, text=True, check=True
    ).stdout.strip()
    return remote.split("github.com")[-1].lstrip(":/").split("/")[0]


def token() -> str:
    """A token with `read:packages` on the org, from the environment first."""
    from_environment = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if from_environment:
        return from_environment
    result = subprocess.run(["gh", "auth", "token"], capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit("error: no GH_TOKEN set and `gh auth token` failed; run `gh auth login`")
    return result.stdout.strip()


def get(path: str, auth_token: str) -> tuple[int, object]:
    """One GET. A 404 is returned rather than raised: it is a normal answer here."""
    request = urllib.request.Request(
        API + path,
        headers={
            "Authorization": f"Bearer {auth_token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode(errors="replace")


def packages_of_type(org: str, package_type: str, auth_token: str) -> list[dict] | None:
    """Every package of one type, or None when the listing failed.

    A failure is reported rather than treated as "no packages": a token that cannot read
    them would otherwise make this check pass by seeing nothing.
    """
    found: list[dict] = []
    page = 1
    while True:
        status, body = get(f"/orgs/{org}/packages?package_type={package_type}&per_page=100&page={page}", auth_token)
        if status != 200:
            print(f"  {package_type}: listing failed with {status}: {str(body)[:160]}", file=sys.stderr)
            return None
        if not isinstance(body, list) or not body:
            return found
        found.extend(body)
        if len(body) < 100:
            return found
        page += 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="exit non-zero when a package is private")
    arguments = parser.parse_args()

    org = organisation()
    auth_token = token()

    private: list[str] = []
    listing_failed = False

    for package_type in PACKAGE_TYPES:
        packages = packages_of_type(org, package_type, auth_token)
        if packages is None:
            listing_failed = True
            continue
        for package in packages:
            name, visibility = package["name"], package["visibility"]
            linked = (package.get("repository") or {}).get("full_name") or "unlinked"
            print(f"  {package_type:9s} {name:34s} {visibility:8s} {linked}")
            if visibility != "public":
                private.append(f"{package_type}/{name}")

    if listing_failed:
        print("error: some package types could not be listed; the report is incomplete", file=sys.stderr)
        return 2

    if not private:
        print(f"OK: every {org} package is public")
        return 0

    print("")
    for package in private:
        print(f"PRIVATE {package}")
    print("")
    print("GitHub has no API for changing a package's visibility. A package published by a")
    print("workflow with GITHUB_TOKEN is public and linked to the repository; one pushed from a")
    print("laptop with a personal token is private and unlinked, and stays that way.")
    print("")
    print("Remedy: delete the package and let the publishing workflow recreate it —")
    print("  gh api --method DELETE /orgs/%s/packages/container/<url-encoded name>" % org)
    print("  then re-run the workflow that publishes it.")
    return 1 if arguments.check else 0


if __name__ == "__main__":
    raise SystemExit(main())
