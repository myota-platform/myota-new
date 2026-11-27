#!/usr/bin/env python3
"""Generate the gateway's route table from the contract.

The gateway must not keep its own copy of which prefix belongs to which service.
Two copies drift, and the copy that drifts silently is the one that proxies
`/v1/entities/` to `activity`. So the table is generated:

    contracts/{core,geodata,activity}.openapi.yaml
                 │  prefix, security requirements per operation
                 ▼
    tools/gen_gateway_routes.py
                 │
                 ▼
    apps/gateway/internal/routes/table_gen.go

`--check` regenerates in memory and fails when the committed file differs, so a
contract change that adds a prefix cannot leave the gateway unrouted.

## Why the audience class is `/v1/admin/` and nothing else

An earlier version derived the class from the contract — "admin if every credential
is an admin credential" — and it produced two wrong answers:

  - `/v1/admin/auth/` came out *anonymous*, because login is anonymous. A
    participant cookie could then reach the MFA enrolment endpoint.
  - `/v1/entities/` came out *admin*, because its only credentialled operations are
    administrator writes. A participant browsing the map with a session cookie
    would have been refused on a route the contract marks anonymous.

Both are the same mistake: a prefix is coarser than the operations under it, so no
per-prefix derivation can be right in both directions. What *is* true, and is what
the architecture states, is that `/v1/admin/**` is the administrator surface and a
participant credential must never be accepted there. That is the one rule the edge
enforces, and the generator proves it still holds rather than assuming it:

  every operation under `/v1/admin/` either requires no credential (login, the MFA
  challenge) or accepts only administrator credentials.

If someone adds a participant-reachable operation under `/v1/admin/`, generation
fails. Everywhere else the edge forwards and the service decides — scope checks
and ownership belong where the data is.

## Token-free paths

The operations under an admin prefix that the contract marks `security: []` are
listed explicitly, because "you cannot present an admin token to a login form" is a
rule the gateway has to know and the contract already states. They are exact paths,
not a wildcard: `/v1/admin/auth/login` is token-free, `/v1/admin/auth/refresh` is
not.

Usage:
    python3 tools/gen_gateway_routes.py --write
    python3 tools/gen_gateway_routes.py --check
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compose_contracts as composer

OUTPUT = composer.REPO_ROOT / "apps" / "gateway" / "internal" / "routes" / "table_gen.go"

# Which audience each security scheme belongs to. A scheme missing here is an error
# rather than an assumption: an unknown scheme classified as "either" would quietly
# widen access on the administrator surface.
SCHEME_AUDIENCE = {
    "participantBearer": "participant",
    "participantSession": "participant",
    "adminBearer": "admin",
    "adminSession": "admin",
}

ADMIN_PREFIX = "/v1/admin/"


def credential_audiences(operation: dict) -> set[str]:
    """The audiences an operation accepts. Empty for a token-free operation."""
    requirements = operation.get("security")
    if requirements is None:
        # No `security` key means it inherits the document default. Every operation
        # in these documents states one explicitly, and inheriting here would be the
        # one place the generator could guess.
        raise SystemExit(
            f"operation {operation.get('operationId')!r} has no explicit `security`; "
            f"write `security: []` for a token-free operation"
        )
    audiences: set[str] = set()
    for requirement in requirements:
        for scheme in requirement:
            audience = SCHEME_AUDIENCE.get(scheme)
            if audience is None:
                raise SystemExit(f"security scheme {scheme!r} has no audience in SCHEME_AUDIENCE")
            audiences.add(audience)
    return audiences


def build_table() -> tuple[list[dict], list[str]]:
    documents = {
        name: composer.load_yaml(composer.service_document_path(name))
        for name in composer.SERVICE_NAMES
    }

    routes, problems = composer.build_route_table(documents)
    if problems:
        return [], problems

    token_free: dict[str, list[str]] = {}
    for service_name in composer.SERVICE_NAMES:
        for path, _method, operation in composer.iter_operations(documents[service_name]):
            if composer.is_internal_operation(operation):
                continue
            prefix = composer.route_prefix(path)
            if prefix is None:
                continue
            audiences = credential_audiences(operation)
            if path.startswith(ADMIN_PREFIX):
                # The invariant: the administrator surface accepts administrator
                # credentials or nothing. A participant credential here is the
                # escalation the two-audience split exists to prevent.
                if "participant" in audiences:
                    problems.append(
                        f"{path} is under {ADMIN_PREFIX} but accepts a participant "
                        f"credential; the gateway would refuse every request the "
                        f"contract says it should serve"
                    )
                if not audiences:
                    token_free.setdefault(prefix, []).append(path)

    table = []
    for prefix in sorted(routes):
        table.append(
            {
                "prefix": prefix,
                "upstream": routes[prefix],
                # The one rule. Not derived per prefix, because no per-prefix
                # derivation is right for both /v1/admin/auth/ and /v1/entities/.
                "audience": "admin" if prefix.startswith(ADMIN_PREFIX) else "open",
                "token_free": sorted(token_free.get(prefix, [])),
            }
        )
    return table, problems


def render(table: list[dict]) -> str:
    lines = [
        "// Code generated by tools/gen_gateway_routes.py from contracts/*.openapi.yaml.",
        "// DO NOT EDIT. Run `python3 tools/gen_gateway_routes.py --write`.",
        "",
        "package routes",
        "",
        "// Table maps every published API prefix to the service that serves it, the",
        "// audience the edge requires for it, and the exact paths under an administrator",
        "// prefix that are reachable without a token.",
        "//",
        "// Sorted by prefix so the file diffs readably and a regenerated table is",
        "// byte-identical when nothing changed.",
        "var Table = []Route{",
    ]
    for row in table:
        token_free = ""
        if row["token_free"]:
            entries = ", ".join(f'"{path}"' for path in row["token_free"])
            token_free = f"TokenFree: []string{{{entries}}}, "
        lines.append(
            f'\t{{Prefix: "{row["prefix"]}", Upstream: "{row["upstream"]}", '
            f"{token_free}Audience: {audience_constant(row['audience'])}}},"
        )
    lines.append("}")
    return "\n".join(lines) + "\n"


def audience_constant(audience: str) -> str:
    return "AudienceAdmin" if audience == "admin" else "AudienceOpen"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--write", action="store_true", help="regenerate table_gen.go")
    action.add_argument("--check", action="store_true", help="fail if table_gen.go is stale")
    args = parser.parse_args()

    table, problems = build_table()
    if problems:
        for problem in problems:
            print(f"  error  {problem}", file=sys.stderr)
        return 1

    rendered = render(table)

    if args.write:
        OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        OUTPUT.write_text(rendered, encoding="utf-8")
        print(f"wrote {OUTPUT.relative_to(composer.REPO_ROOT)}: {len(table)} prefix(es)")
        return 0

    if not OUTPUT.exists():
        print(
            f"  error  {OUTPUT.relative_to(composer.REPO_ROOT)} does not exist; "
            f"run 'python3 tools/gen_gateway_routes.py --write'",
            file=sys.stderr,
        )
        return 1
    if OUTPUT.read_text(encoding="utf-8") != rendered:
        print(
            f"  error  {OUTPUT.relative_to(composer.REPO_ROOT)} is stale; "
            f"run 'python3 tools/gen_gateway_routes.py --write' and commit the result",
            file=sys.stderr,
        )
        return 1

    print(f"OK: {len(table)} prefix(es) in the generated route table")
    return 0


if __name__ == "__main__":
    sys.exit(main())
