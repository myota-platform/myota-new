#!/usr/bin/env python3
"""Check and provision Myota's object storage on Silo.

Silo is the S3-compatible store already running on this cluster. Myota is one tenant
among several there, so this tool is deliberately narrow: it knows about three
buckets per environment, one user per environment, and one policy per user, and it
will not create, delete or inspect anything else.

Modes:

    python3 tools/provision_silo.py --check     # offline; what `make verify` runs
    python3 tools/provision_silo.py --print     # emit the mcli commands to run
    python3 tools/provision_silo.py --live      # read-only comparison with the server

`--check` validates three things against each other:

    deploy/silo/inventory.yaml              the source of truth
    deploy/silo/policies/*.json             what the credential may do
    docs/architecture/object-storage.md     what the document claims

That third one is why this exists. A bucket added to the inventory and not to the
document is a bucket nobody provisions; a policy edit that quietly allows deleting
certificates is a guarantee lost with no diff to review.

`--live` needs `kubectl` and reads through `mcli` inside the Silo pod. It is
read-only, prints what exists and what does not, and never mutates. Provisioning is
`deploy/silo/README.md`.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
INVENTORY = REPO_ROOT / "deploy" / "silo" / "inventory.yaml"
POLICIES = REPO_ROOT / "deploy" / "silo" / "policies"
DOC = REPO_ROOT / "docs" / "architecture" / "object-storage.md"

# The guarantees stated in the document, enforced here.
#
# Each entry is an action that must NOT appear in a policy, with the reason. Adding
# one of these to a policy is not a policy change — it is the loss of a property the
# architecture claims, and it should fail a build rather than be noticed later.
FORBIDDEN_ACTIONS = {
    "s3:*": "wildcards make the rest of this list unreadable and the policy unreviewable",
    "s3:ListAllMyBuckets": "the credential must not be able to see other tenants' buckets",
    "s3:CreateBucket": "buckets are provisioned by an operator, not by the application",
    "s3:DeleteBucket": "an application must never remove a bucket",
    "s3:PutBucketPolicy": "only an operator decides who may read a bucket anonymously",
    "s3:PutBucketAcl": "as above; presigned URLs are the only access path",
    "s3:PutObjectAcl": "a service must not be able to make an object public",
    "admin:*": "no user, policy or configuration management from a service credential",
}

# Statements that must exist, keyed by the action they must not grant. The
# certificate class is the one irreplaceable set of bytes in the system, so the
# credential that writes it may not delete it.
WRITE_ONLY_PREFIXES = {
    "certificates": {"s3:GetObject", "s3:PutObject"},
}


class Problem(Exception):
    """A check failed. Carries every message found, not the first."""

    def __init__(self, messages: list[str]):
        super().__init__("\n".join(messages))
        self.messages = messages


def load_inventory() -> dict:
    if not INVENTORY.exists():
        raise Problem([f"{INVENTORY.relative_to(REPO_ROOT)} does not exist"])
    return yaml.safe_load(INVENTORY.read_text(encoding="utf-8"))


def load_policy(name: str) -> dict:
    path = POLICIES / f"{name}.json"
    if not path.exists():
        raise Problem([f"{path.relative_to(REPO_ROOT)} does not exist"])
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise Problem([f"{path.name} is not valid JSON: {error}"]) from error


def bucket_names(inventory: dict, environment: str) -> list[str]:
    return [bucket["name"] for bucket in inventory["environments"][environment]["buckets"]]


def check_naming(inventory: dict) -> list[str]:
    """Every bucket carries its environment, and no two environments share one."""
    problems: list[str] = []
    seen: dict[str, str] = {}
    for environment, settings in inventory["environments"].items():
        for bucket in settings["buckets"]:
            name = bucket["name"]
            if not name.startswith(f"myota-{environment}-"):
                problems.append(
                    f"{name}: a bucket name must start with 'myota-{environment}-' so it "
                    f"cannot be confused with the other environment's"
                )
            if name in seen:
                problems.append(f"{name}: used by both {seen[name]} and {environment}")
            seen[name] = environment
        user = settings["user"]
        if not user.startswith(f"myota-{environment}"):
            problems.append(f"user {user} does not carry the environment in its name")
        if settings["policy"] != f"{user}-only":
            problems.append(
                f"policy {settings['policy']} does not follow the '<user>-only' convention "
                f"this server already uses for bminfo"
            )
    return problems


def check_policy_actions(inventory: dict, environment: str, policy: dict) -> list[str]:
    """The policy grants what it should, and nothing the document says it must not."""
    problems: list[str] = []
    label = inventory["environments"][environment]["policy"]
    expected_buckets = set(bucket_names(inventory, environment))
    granted_buckets: set[str] = set()

    for statement in policy.get("Statement") or []:
        actions = statement.get("Action") or []
        if isinstance(actions, str):
            actions = [actions]
        resources = statement.get("Resource") or []
        if isinstance(resources, str):
            resources = [resources]

        for action in actions:
            if action in FORBIDDEN_ACTIONS:
                problems.append(f"{label}: {statement.get('Sid')} grants {action} — {FORBIDDEN_ACTIONS[action]}")
            elif action.startswith("admin:") or (action.endswith(":*") and action != "s3:*"):
                problems.append(f"{label}: {statement.get('Sid')} grants the wildcard {action}")

        for resource in resources:
            if not resource.startswith("arn:aws:s3:::"):
                problems.append(f"{label}: {statement.get('Sid')} has a resource that is not an S3 ARN: {resource}")
                continue
            remainder = resource[len("arn:aws:s3:::") :]
            bucket = remainder.split("/")[0]
            granted_buckets.add(bucket)
            if bucket not in expected_buckets:
                problems.append(
                    f"{label}: {statement.get('Sid')} reaches {bucket}, which is not one of "
                    f"this environment's buckets ({', '.join(sorted(expected_buckets))})"
                )

    missing = expected_buckets - granted_buckets
    if missing:
        problems.append(f"{label}: no statement mentions {', '.join(sorted(missing))}")

    # The delete-protection guarantee, asserted rather than described.
    for prefix, allowed in WRITE_ONLY_PREFIXES.items():
        for statement in policy.get("Statement") or []:
            resources = statement.get("Resource") or []
            if isinstance(resources, str):
                resources = [resources]
            if not any(f"/{prefix}/" in resource for resource in resources):
                continue
            actions = statement.get("Action") or []
            if isinstance(actions, str):
                actions = [actions]
            if "s3:DeleteObject" in actions:
                problems.append(
                    f"{label}: {statement.get('Sid')} can delete under {prefix}/ — the document "
                    f"says {prefix} is write-once for the service credential"
                )
            unexpected = set(actions) - allowed
            if unexpected:
                problems.append(
                    f"{label}: {statement.get('Sid')} grants {', '.join(sorted(unexpected))} under "
                    f"{prefix}/, where the documented set is {', '.join(sorted(allowed))}"
                )
    return problems


def check_policy_covers_inventory(inventory: dict, environment: str) -> list[str]:
    """Every declared prefix is reachable by the policy, and no undeclared one."""
    problems: list[str] = []
    policy = load_policy(inventory["environments"][environment]["policy"])
    label = inventory["environments"][environment]["policy"]
    for bucket in inventory["environments"][environment]["buckets"]:
        for entry in bucket["prefixes"]:
            prefix = entry["prefix"]
            wanted = f"arn:aws:s3:::{bucket['name']}/{prefix}*"
            covered = False
            for statement in policy.get("Statement") or []:
                resources = statement.get("Resource") or []
                if isinstance(resources, str):
                    resources = [resources]
                if any(resource in (wanted, f"arn:aws:s3:::{bucket['name']}/*") for resource in resources):
                    covered = True
            if not covered:
                problems.append(
                    f"{label}: {bucket['name']}/{prefix} is in the inventory and no statement "
                    f"covers it, so {entry['owner']} could not write there"
                )
    return problems


def check_documentation(inventory: dict) -> list[str]:
    """The document names every bucket, user and policy the inventory declares."""
    problems: list[str] = []
    if not DOC.exists():
        return [f"{DOC.relative_to(REPO_ROOT)} does not exist"]
    text = DOC.read_text(encoding="utf-8")

    for environment, settings in inventory["environments"].items():
        for need in [settings["user"], settings["policy"], *bucket_names(inventory, environment)]:
            if need not in text:
                problems.append(f"{DOC.name} never mentions {need!r}, which the inventory requires")

    # The two facts that break silently at runtime rather than loudly at review.
    # Compared with underscores and hyphens folded together, because the document
    # writes "path-style" in prose and `S3_SIGNING_ENDPOINT` in a table, and both
    # count as mentioning it.
    normalised = text.lower().replace("-", "_")
    for fact, why in (
        ("path_style", "virtual-host addressing fails at Traefik's TLS layer"),
        ("signing_endpoint", "a presigned URL signed for the in-cluster host cannot be used by a browser"),
    ):
        if fact not in normalised:
            problems.append(f"{DOC.name} does not mention {fact} ({why})")
    return problems


def print_commands(inventory: dict) -> str:
    """The exact commands, in order, for one environment.

    Kept as a generator rather than prose in the README so the runbook and the
    inventory cannot disagree. The access key and secret are read from the
    environment at run time and are never printed.
    """
    server = inventory["server"]
    alias = server["alias"]
    lines = [
        "# Generated by tools/provision_silo.py --print. Run as a Silo administrator.",
        "# Two kinds of credential appear below and they are not interchangeable:",
        "#   SILO_ADMIN_*          the administrator running these commands",
        "#   MYOTA_<ENV>_S3_SECRET the secret for the user being created",
        "# In Silo a user's name is its access key, so only the secret is supplied here.",
        f'mcli alias set {alias} {server["endpoint"]} "$SILO_ADMIN_ACCESS_KEY" "$SILO_ADMIN_SECRET_KEY"',
    ]
    for environment, settings in inventory["environments"].items():
        lines.append("")
        lines.append(f"# ---- {environment} ({settings['namespace']}) ----")
        for bucket in settings["buckets"]:
            lines.append(
                f"mcli mb --ignore-existing --region {server['region']} {alias}/{bucket['name']}"
            )
            if bucket.get("retain_days"):
                lines.append(
                    f"mcli ilm add --expire-days {bucket['retain_days']} {alias}/{bucket['name']}"
                )
        lines.append(
            f"mcli admin policy create {alias} {settings['policy']} "
            f"deploy/silo/policies/{settings['policy']}.json"
        )
        lines.append(
            f'mcli admin user add {alias} {settings["user"]} '
            f'"$MYOTA_{environment.upper()}_S3_SECRET"'
        )
        lines.append(
            f"mcli admin policy attach {alias} {settings['policy']} --user {settings['user']}"
        )
    return "\n".join(lines)


def check_offline(inventory: dict) -> list[str]:
    problems = check_naming(inventory)
    for environment in inventory["environments"]:
        problems.extend(check_policy_actions(inventory, environment, load_policy(
            inventory["environments"][environment]["policy"])))
        problems.extend(check_policy_covers_inventory(inventory, environment))
    problems.extend(check_documentation(inventory))
    return problems


def check_live(inventory: dict) -> list[str]:
    """Compare the inventory with the server. Read-only; reports, never provisions."""
    problems: list[str] = []
    if shutil.which("kubectl") is None:
        print("  note  kubectl is not on PATH — live comparison skipped", file=sys.stderr)
        return problems

    def mcli(*arguments: str) -> str:
        command = ["kubectl", "-n", "silo", "exec", "deploy/silo", "--", "mcli", *arguments]
        result = subprocess.run(command, capture_output=True, text=True, timeout=60)
        if result.returncode != 0:
            raise Problem([f"mcli {' '.join(arguments)} failed: {result.stderr.strip()}"])
        return result.stdout

    try:
        existing_buckets = {
            line.split()[-1].rstrip("/")
            for line in mcli("ls", "local").splitlines()
            if line.strip()
        }
        existing_users = {
            line.split()[1] for line in mcli("admin", "user", "list", "local").splitlines() if line.strip()
        }
    except (Problem, subprocess.SubprocessError) as error:
        print(f"  note  live comparison unavailable: {error}", file=sys.stderr)
        return problems

    print("  live state on the Silo server:")
    for environment, settings in inventory["environments"].items():
        print(f"    {environment}:")
        for bucket in bucket_names(inventory, environment):
            state = "exists" if bucket in existing_buckets else "MISSING"
            print(f"      bucket {bucket:<24} {state}")
        state = "exists" if settings["user"] in existing_users else "MISSING"
        print(f"      user   {settings['user']:<24} {state}")
    print("  (reported, not a failure — provisioning is deploy/silo/README.md)")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--check", action="store_true", help="offline validation of inventory, policies and docs")
    action.add_argument("--print", action="store_true", help="emit the mcli commands that provision this")
    action.add_argument("--live", action="store_true", help="read-only comparison with the running server")
    args = parser.parse_args()

    try:
        inventory = load_inventory()
        if args.print:
            print(print_commands(inventory))
            return 0
        problems = check_offline(inventory)
        if args.live:
            problems.extend(check_live(inventory))
        if problems:
            raise Problem(problems)
    except Problem as error:
        for problem in error.messages:
            print(f"  error  {problem}", file=sys.stderr)
        return 1

    buckets = sum(len(bucket_names(inventory, environment)) for environment in inventory["environments"])
    users = len(inventory["environments"])
    print(f"OK: {buckets} bucket(s) and {users} user(s) consistent across inventory, policies and document")
    return 0


if __name__ == "__main__":
    sys.exit(main())
