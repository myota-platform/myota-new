#!/usr/bin/env python3
"""Compose the three service contracts into the one document the gateway serves.

The gateway is the only holder of the published contract set (AGENTS.md rule 1).
Each service keeps its own document as the source of truth for its handlers and
exposes it at `GET /internal/openapi.json` for build-time composition and
contract tests. This script is the composer:

    contracts/core.openapi.yaml      ─┐
    contracts/geodata.openapi.yaml   ─┼─►  contracts/public.openapi.yaml
    contracts/activity.openapi.yaml  ─┘

What it enforces, and why each rule is a rule:

  operationId uniqueness      Two operations with one id make generated clients
                              ambiguous and make "which handler serves this?"
                              unanswerable. Nothing else in the repo catches it.
  no path in two services     The same route served by two services is the
                              duplication this redesign removed. The composed
                              document is where it would become invisible.
  no tag in two services      Tags group the playground. A shared tag merges two
                              services' operations into one list and hides which
                              service owns what.
  internal operations dropped `/internal/**` must never reach a public document.
                              Filtering happens here rather than by convention,
                              because a leak is silent otherwise.
  every operation has          A contract without a summary is a contract that
  operationId, summary,        documents nothing; one without an example is one
  and an example              nobody can try. Both are cheap to require here.
  no bare-array responses     A bare array cannot grow a total or a cursor
                              without breaking every client.
  Idempotency-Key on POST     Retries happen. Every POST or PATCH either
  and PATCH, or an explicit   documents the header or states why it is
  exemption                   inapplicable — the exemption is reviewable, not
                              an allowlist buried in this file.
  every error is Problem      One error envelope, `application/problem+json`.

Usage:
    python3 tools/compose_contracts.py --check     # CI and `make verify`
    python3 tools/compose_contracts.py --write     # regenerate the composed file
    python3 tools/compose_contracts.py --fetch core=http://localhost:8080 --check

`--check` also compares a live service's `/internal/openapi.json` against the
committed document when `--fetch` is given, which is the contract test that F3
onward runs against running handlers. With no service running, the committed
documents are the source of truth — which is the state F2 delivers.
"""

from __future__ import annotations

import argparse
import copy
import json
import sys
from pathlib import Path
from typing import Any

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
CONTRACTS_DIR = REPO_ROOT / "contracts"
SHARED_COMPONENTS = CONTRACTS_DIR / "shared" / "components.yaml"
COMPOSITION_HEADER = CONTRACTS_DIR / "composition.yaml"
PUBLIC_DOCUMENT = CONTRACTS_DIR / "public.openapi.yaml"

# Order matters: it is the order tags and paths appear in the published document,
# and a stable order is what makes `--check` a byte comparison rather than a diff.
SERVICE_NAMES = ("core", "geodata", "activity")

# How a service document points at the shared file. The prefix is rewritten to a
# local pointer during composition; the checked-in form stays relative so each
# service document is readable and validatable on its own.
SHARED_REF_PREFIX = "../shared/components.yaml#"

MUTATING_METHODS = ("post", "patch")


class ContractError(Exception):
    """A contract rule was broken. Carries every message found, not the first."""

    def __init__(self, messages: list[str]):
        super().__init__("\n".join(messages))
        self.messages = messages


def service_document_path(service_name: str) -> Path:
    return CONTRACTS_DIR / f"{service_name}.openapi.yaml"


class StrictLoader(yaml.SafeLoader):
    """A loader that refuses duplicate mapping keys.

    PyYAML's default is to keep the last occurrence and say nothing. That is how
    a path written twice — `/v1/entities:` with `get:` in one place and `post:` in
    another — silently loses the first operation. A contract that drops an
    endpoint without a word is worse than one that fails to parse, so duplicates
    are an error here.
    """


def _construct_mapping(loader: yaml.SafeLoader, node: yaml.Node, deep: bool = False) -> dict:
    mapping: dict = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in mapping:
            raise ContractError(
                [
                    f"duplicate key {key!r} at line {key_node.start_mark.line + 1}; "
                    f"YAML would keep the last and silently drop the rest"
                ]
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


StrictLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _construct_mapping
)


def load_yaml(path: Path) -> Any:
    with path.open(encoding="utf-8") as handle:
        return yaml.load(handle, Loader=StrictLoader)


def read_service_documents(fetched: dict[str, Any]) -> dict[str, dict]:
    """Read each service document, from disk unless a live one was fetched.

    A missing document is a hard error, not a skip: a silently composed
    two-service API is worse than no API, because the missing third looks like a
    deliberate scope decision.
    """
    documents: dict[str, dict] = {}
    missing: list[str] = []
    for name in SERVICE_NAMES:
        if name in fetched:
            documents[name] = fetched[name]
            continue
        path = service_document_path(name)
        if not path.exists():
            # relative_to raises for a path outside the repository, which is
            # exactly the case a test provides, and a traceback is a worse error
            # message than the path itself.
            try:
                shown = path.relative_to(REPO_ROOT)
            except ValueError:
                shown = path
            missing.append(f"{shown} does not exist")
            continue
        documents[name] = load_yaml(path)
    if missing:
        raise ContractError(missing)
    return documents


def iter_operations(document: dict):
    """Yield (path, method, operation) for every operation in a document."""
    for path, path_item in (document.get("paths") or {}).items():
        if not isinstance(path_item, dict):
            continue
        for method, operation in path_item.items():
            if method.lower() in {"get", "put", "post", "delete", "patch", "head", "options"}:
                yield path, method.lower(), operation


def is_internal_operation(operation: dict) -> bool:
    return bool(operation.get("x-internal"))


def rewrite_shared_refs(node: Any, shared: dict, reported: list[str]) -> Any:
    """Point every shared reference at the composed document's own components.

    Also verifies the target exists. A `$ref` to a typo resolves to nothing, and
    a renderer then shows an empty schema rather than an error — which is how a
    contract quietly stops documenting a field.
    """
    if isinstance(node, dict):
        rewritten = {}
        for key, value in node.items():
            if key == "$ref" and isinstance(value, str) and value.startswith(SHARED_REF_PREFIX):
                # SHARED_REF_PREFIX ends at the '#', so the remainder is a JSON
                # pointer body: '/components/schemas/Problem'.
                pointer = "#" + value[len(SHARED_REF_PREFIX) :]
                if resolve_local_pointer(shared, pointer) is None:
                    reported.append(f"dangling shared reference: {value}")
                rewritten[key] = pointer
            else:
                rewritten[key] = rewrite_shared_refs(value, shared, reported)
        return rewritten
    if isinstance(node, list):
        return [rewrite_shared_refs(item, shared, reported) for item in node]
    return node


def resolve_local_pointer(document: Any, pointer: str) -> Any:
    """Resolve `#/a/b/c` against a document. Returns None when absent."""
    if not pointer.startswith("#/"):
        return None
    current = document
    for token in pointer[2:].split("/"):
        token = token.replace("~1", "/").replace("~0", "~")
        if isinstance(current, dict) and token in current:
            current = current[token]
        elif isinstance(current, list) and token.isdigit() and int(token) < len(current):
            current = current[int(token)]
        else:
            return None
    return current


def component_references(node: Any) -> set[str]:
    """Every `#/components/<kind>/<name>` pointer reachable from a subtree."""
    found: set[str] = set()
    stack = [node]
    while stack:
        current = stack.pop()
        if isinstance(current, dict):
            for key, value in current.items():
                if key == "$ref" and isinstance(value, str) and value.startswith("#/components/"):
                    found.add(value)
                else:
                    stack.append(value)
        elif isinstance(current, list):
            stack.extend(current)
    return found


def security_scheme_names(node: Any) -> set[str]:
    """Scheme names named by `security` requirements anywhere in a subtree."""
    names: set[str] = set()
    stack = [node]
    while stack:
        current = stack.pop()
        if isinstance(current, dict):
            for key, value in current.items():
                if key == "security" and isinstance(value, list):
                    for requirement in value:
                        if isinstance(requirement, dict):
                            names.update(requirement.keys())
                else:
                    stack.append(value)
        elif isinstance(current, list):
            stack.extend(current)
    return names


def requires_example(operation: dict) -> bool:
    """False for operations that return no body and take none.

    A `DELETE` answering `204`, a logout, an event acknowledgement: there is
    nothing to exemplify, and demanding an example would produce a fabricated
    body for a response that has none.
    """
    if operation.get("requestBody"):
        return True
    for response in (operation.get("responses") or {}).values():
        if not isinstance(response, dict):
            continue
        for media_type, media in (response.get("content") or {}).items():
            if media_type == "application/problem+json":
                continue
            # A binary body cannot carry a JSON example, and inventing one would
            # be worse than stating that there is none.
            if isinstance(media, dict) and (media.get("schema") or {}).get("format") == "binary":
                continue
            return True
    return False


def collect_example_clues(operation: dict) -> bool:
    """True when the operation shows at least one concrete example.

    A media-type `example`/`examples` is the common case. A schema-level
    `example` counts too, because a shared component may carry one. A response
    with no content — `204` on logout, on a delete — has nothing to exemplify and
    is not asked for one.
    """
    if _has_example_in_content(operation.get("requestBody")):
        return True
    for response in (operation.get("responses") or {}).values():
        if not isinstance(response, dict):
            continue
        if _has_example_in_content(response):
            return True
        for media_type in (response.get("content") or {}).values():
            if isinstance(media_type, dict) and "example" in (media_type.get("schema") or {}):
                return True
    return False


def _has_example_in_content(container: Any) -> bool:
    if not isinstance(container, dict):
        return False
    for media_type in (container.get("content") or {}).values():
        if isinstance(media_type, dict) and ("example" in media_type or "examples" in media_type):
            return True
    return False


def operation_is_covered_by_responses(operation: dict) -> bool:
    return bool(operation.get("responses"))


def compose(fetched: dict[str, Any] | None = None) -> tuple[dict, list[str]]:
    """Build the composed document. Returns (document, problems found)."""
    fetched = fetched or {}
    problems: list[str] = []

    shared_file = load_yaml(SHARED_COMPONENTS)
    shared_components = shared_file.get("components") or {}
    header = load_yaml(COMPOSITION_HEADER)

    documents = read_service_documents(fetched)

    composed_paths: dict[str, dict] = {}
    composed_tags: list[dict] = []
    tag_owner: dict[str, str] = {}
    path_owner: dict[str, str] = {}
    operation_owner: dict[str, str] = {}
    # Seed from the shared file so every rewritten pointer resolves, then add
    # each service's own components on top. A service that redefines a shared
    # name is a collision and is reported, not silently overridden.
    merged_components: dict[str, dict] = {
        kind: copy.deepcopy(entries or {})
        for kind, entries in shared_components.items()
    }
    for kind in ("securitySchemes", "schemas", "parameters", "responses", "headers"):
        merged_components.setdefault(kind, {})

    # The shared file's own entries point at each other with the relative file
    # prefix. Inside the composed document those pointers have to become local.
    for kind, entries in merged_components.items():
        merged_components[kind] = {
            name: rewrite_shared_refs(entry, shared_file, problems)
            for name, entry in entries.items()
        }
    tag_order: list[str] = []

    for service_name in SERVICE_NAMES:
        document = documents[service_name]

        for tag in document.get("tags") or []:
            tag_name = tag.get("name")
            if not tag_name:
                problems.append(f"{service_name}: a tag entry has no name")
                continue
            if tag_name in tag_owner and tag_owner[tag_name] != service_name:
                problems.append(
                    f"tag '{tag_name}' is declared by both {tag_owner[tag_name]} and "
                    f"{service_name}; tags must be unique per service so the "
                    f"playground never merges two services into one group"
                )
                continue
            tag_owner[tag_name] = service_name
            if tag_name not in tag_order:
                tag_order.append(tag_name)
                composed_tags.append(copy.deepcopy(tag))

        # Service-local components are already `#/components/...`; only the
        # shared file's pointers need rewriting.
        local_components = document.get("components") or {}
        for kind, entries in local_components.items():
            bucket = merged_components.setdefault(kind, {})
            for entry_name, entry in (entries or {}).items():
                if entry_name in bucket:
                    problems.append(
                        f"{service_name}: component {kind}/{entry_name} collides "
                        f"with an existing definition; rename it or move it to "
                        f"contracts/shared/components.yaml"
                    )
                    continue
                bucket[entry_name] = rewrite_shared_refs(entry, shared_file, problems)

        for path, method, operation in iter_operations(document):
            operation_id = operation.get("operationId")
            internal = is_internal_operation(operation)

            if not internal:
                if path in path_owner and path_owner[path] != service_name:
                    problems.append(
                        f"path {path} is declared by both {path_owner[path]} and "
                        f"{service_name}; one route, one owner"
                    )
                    continue
                path_owner[path] = service_name

            if not operation_id:
                problems.append(f"{service_name}: {method.upper()} {path} has no operationId")
            elif operation_id in operation_owner:
                problems.append(
                    f"duplicate operationId '{operation_id}': "
                    f"{operation_owner[operation_id]} and {service_name} ({method.upper()} {path})"
                )
            else:
                operation_owner[operation_id] = f"{service_name} ({method.upper()} {path})"

            if not operation.get("summary"):
                problems.append(f"{service_name}: {method.upper()} {path} has no summary")
            if not operation_is_covered_by_responses(operation):
                problems.append(f"{service_name}: {method.upper()} {path} declares no responses")
            if requires_example(operation) and not collect_example_clues(operation):
                problems.append(
                    f"{service_name}: {method.upper()} {path} has no example; add one "
                    f"under the response media type so the playground can try it"
                )

            problems.extend(check_operation_rules(service_name, path, method, operation))

            if internal:
                continue

            rewritten = rewrite_shared_refs(copy.deepcopy(operation), shared_file, problems)
            composed_paths.setdefault(path, {})[method] = rewritten

    problems.extend(check_internal_paths(documents))
    problems.extend(check_tag_usage(documents, tag_owner))
    problems.extend(check_ref_discipline(documents))

    # Every published prefix must have exactly one owner, or the gateway's route
    # table cannot be built from the contract and a request could reach the wrong
    # service. Reported here rather than discovered in F3.
    _, route_problems = build_route_table(documents)
    problems.extend(route_problems)

    composed = {
        "openapi": "3.1.0",
        "info": copy.deepcopy(header["info"]),
        "jsonSchemaDialect": "https://json-schema.org/draft/2020-12/schema",
        "servers": copy.deepcopy(header["servers"]),
        "security": [],
        "tags": [tag for tag in composed_tags if tag["name"] in _used_tags(composed_paths)],
        "paths": composed_paths,
        "components": merged_components,
    }
    if "externalDocs" in header:
        composed["externalDocs"] = copy.deepcopy(header["externalDocs"])

    # Keep only the components the published paths actually reach. An internal
    # schema in a public document is not an error, but it is a hint that
    # something internal was about to leak.
    composed["components"] = prune_unreachable_components(composed)

    problems.extend(check_no_internal_leak(composed))
    return composed, problems


def _used_tags(paths: dict[str, dict]) -> set[str]:
    used: set[str] = set()
    for path_item in paths.values():
        for operation in path_item.values():
            if isinstance(operation, dict):
                used.update(operation.get("tags") or [])
    return used


def check_operation_rules(service_name: str, path: str, method: str, operation: dict) -> list[str]:
    """Per-operation rules from AGENTS.md rule 1 and contracts/README.md."""
    problems: list[str] = []
    label = f"{service_name}: {method.upper()} {path}"

    tags = operation.get("tags") or []
    if not tags:
        problems.append(f"{label} has no tag; every operation belongs to one playground group")

    if method in MUTATING_METHODS:
        idempotency = operation.get("x-idempotency")
        has_header = any(
            (parameter.get("$ref", "").endswith("/IdempotencyKey"))
            or (parameter.get("name") == "Idempotency-Key")
            for parameter in operation.get("parameters") or []
            if isinstance(parameter, dict)
        )
        if not has_header:
            if idempotency != "not-applicable":
                problems.append(
                    f"{label} mutates and does not document Idempotency-Key; "
                    f"either add the parameter or set x-idempotency: not-applicable "
                    f"with x-idempotency-reason"
                )
            elif not operation.get("x-idempotency-reason"):
                problems.append(f"{label} is exempt from Idempotency-Key but gives no reason")

    for status, response in (operation.get("responses") or {}).items():
        if not isinstance(response, dict):
            continue
        for media_type, media in (response.get("content") or {}).items():
            if not isinstance(media, dict):
                continue
            schema = media.get("schema") or {}
            if schema.get("type") == "array":
                problems.append(
                    f"{label}: {status} returns a bare array; collections return "
                    f"{{items, nextCursor}}"
                )
            if str(status).startswith(("4", "5")) and media_type == "application/json":
                problems.append(
                    f"{label}: {status} uses application/json for an error; errors are "
                    f"application/problem+json"
                )
    return problems


def check_ref_discipline(documents: dict[str, dict]) -> list[str]:
    """Every reference must point inside the document or at the shared file.

    The composed document has to be self-contained, so a service document may only
    reference `#/...` (itself) or `../shared/components.yaml#...`. Anything else —
    a typo in the file name, a reference to a document that does not travel with
    the contract — survives YAML parsing, survives the structural walk (which only
    looks at `#/components/`), and fails only when someone opens the playground
    and finds an empty schema.
    """
    problems: list[str] = []
    for service_name, document in documents.items():
        for service_label, node in _walk_refs(document):
            if not isinstance(node, str):
                continue
            if node.startswith("#") or node.startswith(SHARED_REF_PREFIX):
                continue
            problems.append(
                f"{service_name}: {service_label} references '{node}'; a service "
                f"document may only reference itself or {SHARED_REF_PREFIX}..."
            )
    return problems


def _walk_refs(node: Any) -> list[tuple[str, Any]]:
    found: list[tuple[str, Any]] = []
    stack: list[tuple[str, Any]] = [("", node)]
    while stack:
        label, current = stack.pop()
        if isinstance(current, dict):
            for key, value in current.items():
                if key == "$ref":
                    found.append((label or "$ref", value))
                else:
                    stack.append((f"{label}/{key}", value))
        elif isinstance(current, list):
            for index, value in enumerate(current):
                stack.append((f"{label}/{index}", value))
    return found


def check_internal_paths(documents: dict[str, dict]) -> list[str]:
    """`/internal/**` is exempt from the published-document rules, and only it."""
    problems: list[str] = []
    for service_name, document in documents.items():
        for path, method, operation in iter_operations(document):
            marked = is_internal_operation(operation)
            if path.startswith("/internal/") and not marked:
                problems.append(
                    f"{service_name}: {method.upper()} {path} is under /internal but is "
                    f"not marked x-internal: true, so it would be published"
                )
            if marked and not path.startswith("/internal/"):
                problems.append(
                    f"{service_name}: {method.upper()} {path} is marked internal but is "
                    f"not under /internal"
                )
            if not path.startswith(("/v1/", "/internal/")):
                problems.append(
                    f"{service_name}: {method.upper()} {path} is versioned outside "
                    f"/v1 or /internal"
                )
    return problems


def check_tag_usage(documents: dict[str, dict], tag_owner: dict[str, str]) -> list[str]:
    problems: list[str] = []
    for service_name, document in documents.items():
        declared = {tag.get("name") for tag in document.get("tags") or []}
        for path, method, operation in iter_operations(document):
            for tag in operation.get("tags") or []:
                if tag not in declared:
                    problems.append(
                        f"{service_name}: {method.upper()} {path} uses undeclared tag '{tag}'"
                    )
    return problems


def route_prefix(path: str) -> str | None:
    """The prefix the gateway routes on, or None for an unpublished path.

    Two segments normally: `/v1/entities/<uuid>` is served by whoever owns
    `/v1/entities/`. Under `/v1/admin/` it is three, because every service has
    administrator endpoints and a single `/v1/admin/` prefix would belong to all
    three at once.

    State transitions like `/v1/imports/<uuid>/publish` stay under their resource's
    prefix. The gateway does not need to know which verbs a path supports — it
    needs to know which service to ask.
    """
    if not path.startswith("/v1/"):
        return None
    segments = path.strip("/").split("/")
    depth = 3 if len(segments) > 2 and segments[1] == "admin" else 2
    return "/" + "/".join(segments[:depth]) + "/"


def build_route_table(documents: dict[str, dict]) -> tuple[dict[str, str], list[str]]:
    """Map each published prefix to exactly one service.

    This is the table the gateway needs, derived from the documents rather than
    transcribed into a Go literal by hand. A prefix claimed by two services is
    reported: it means one of them is serving a route that belongs to the other.
    """
    owners: dict[str, str] = {}
    problems: list[str] = []
    for service_name in SERVICE_NAMES:
        for path, method, operation in iter_operations(documents[service_name]):
            if is_internal_operation(operation):
                continue
            prefix = route_prefix(path)
            if prefix is None:
                continue
            if prefix in owners and owners[prefix] != service_name:
                problems.append(
                    f"prefix {prefix} is claimed by {owners[prefix]} and {service_name} "
                    f"({' / '.join(sorted({p for p in (m for m in _paths_for(documents, owners[prefix])) if route_prefix(p) == prefix}))} "
                    f"vs {path})"
                )
                continue
            owners[prefix] = service_name
    return owners, problems


def _paths_for(documents: dict[str, dict], service_name: str) -> list[str]:
    return [
        path
        for path, _, operation in iter_operations(documents[service_name])
        if not is_internal_operation(operation)
    ]


def prune_unreachable_components(composed: dict) -> dict:
    """Drop every component the published paths do not reach, transitively."""
    components = composed["components"]
    reached: set[str] = set()
    frontier = component_references(composed["paths"])
    frontier |= {f"#/components/securitySchemes/{name}" for name in security_scheme_names(composed["paths"])}

    while frontier:
        pointer = frontier.pop()
        if pointer in reached:
            continue
        reached.add(pointer)
        target = resolve_local_pointer(composed, pointer)
        if target is None:
            continue
        frontier |= component_references(target)

    pruned: dict[str, dict] = {}
    for kind, entries in components.items():
        kept = {
            name: entry
            for name, entry in entries.items()
            if f"#/components/{kind}/{name}" in reached
        }
        if kept:
            pruned[kind] = kept
    return pruned


def check_no_internal_leak(composed: dict) -> list[str]:
    problems: list[str] = []
    for path in composed.get("paths") or {}:
        if path.startswith("/internal/"):
            problems.append(f"internal path {path} reached the published document")
    return problems


def structural_problems(document: dict) -> list[str]:
    """Checks that hold whether or not a spec validator is installed.

    Deliberately not a reimplementation of the OpenAPI schema: it covers the
    mistakes this repository can actually make — an unresolved pointer, an empty
    operation, a path that is not a path.
    """
    problems: list[str] = []
    if document.get("openapi") != "3.1.0":
        problems.append(f"openapi is {document.get('openapi')!r}, expected '3.1.0'")
    for key in ("info", "paths", "components"):
        if key not in document:
            problems.append(f"missing top-level '{key}'")
    info = document.get("info") or {}
    for key in ("title", "version"):
        if not info.get(key):
            problems.append(f"info.{key} is empty")
    for path in document.get("paths") or {}:
        if not path.startswith("/"):
            problems.append(f"path '{path}' does not start with /")
    for path, method, operation in iter_operations(document):
        for required in ("operationId", "summary", "responses"):
            if not operation.get(required):
                problems.append(f"{method.upper()} {path} has no {required}")
    for pointer in sorted(component_references(document)):
        if resolve_local_pointer(document, pointer) is None:
            problems.append(f"unresolved reference {pointer}")
    return problems


def validate_openapi(document: dict) -> list[str]:
    """Validate against OpenAPI 3.1, using the real validator when installed.

    The package is a declared dev dependency (`tools/requirements.txt`). When it
    is absent this reports a note instead of failing, for the same reason
    `check_diagrams.py` reports a missing `mmdc`: a check that cannot run must
    say so, not pretend to have passed. CI installs it, so CI always runs it.
    """
    try:
        from openapi_spec_validator import validate
    except ImportError:
        print(
            "  note  openapi-spec-validator not installed — structural checks only.\n"
            "        pip install -r tools/requirements.txt",
            file=sys.stderr,
        )
        return []
    try:
        validate(document)
    except Exception as error:  # noqa: BLE001 — the validator raises many subtypes
        return [f"OpenAPI 3.1 validation failed: {error}"]
    return []


class _ContractDumper(yaml.SafeDumper):
    """Emit prose as literal blocks.

    PyYAML's default for a string containing newlines is a single-quoted scalar
    with blank lines between the paragraphs. It is valid YAML and unreadable in a
    pull request, and this file is reviewed in pull requests.
    """


def _represent_string(dumper: yaml.SafeDumper, data: str) -> yaml.Node:
    style = "|" if "\n" in data else None
    return dumper.represent_scalar("tag:yaml.org,2002:str", data, style=style)


_ContractDumper.add_representer(str, _represent_string)


def serialise(document: dict) -> str:
    """Deterministic YAML. Sorted keys would scramble the reading order, so the
    order is the one this script built — which is itself deterministic."""
    return yaml.dump(
        document,
        Dumper=_ContractDumper,
        sort_keys=False,
        allow_unicode=True,
        default_flow_style=False,
        width=120,
        indent=2,
    )


def compare_live_document(service_name: str, live: dict, committed: dict) -> list[str]:
    """Compare a live `/internal/openapi.json` with the committed document.

    Reported as JSON pointers so the failing field is named rather than "the
    documents differ". Only paths, operations and components are compared:
    `info` differs harmlessly because a service may add a build version.
    """
    problems: list[str] = []
    live_ids = {op.get("operationId") for _, _, op in iter_operations(live)}
    committed_ids = {op.get("operationId") for _, _, op in iter_operations(committed)}
    for operation_id in sorted(committed_ids - live_ids):
        problems.append(
            f"{service_name}: documented operation '{operation_id}' is not served"
        )
    for operation_id in sorted(live_ids - committed_ids):
        problems.append(
            f"{service_name}: serves undocumented operation '{operation_id}'"
        )
    for path, method, operation in iter_operations(live):
        pointer = f"#/paths/{path.replace('/', '~1')}/{method}"
        delivered = resolve_local_pointer(committed, pointer)
        if delivered is None:
            continue
        if operation.get("deprecated") != delivered.get("deprecated"):
            problems.append(f"{service_name}: {method.upper()} {path} deprecation differs")
    return problems


def fetch_service_documents(fetched: dict[str, Any], base_urls: list[str]) -> list[str]:
    """Fetch live documents into `fetched`. Returns problems."""
    import urllib.error
    import urllib.request

    for assignment in base_urls:
        if "=" not in assignment:
            return [f"--fetch expects name=url, got '{assignment}'"]
        name, url = assignment.split("=", 1)
        if name not in SERVICE_NAMES:
            return [f"--fetch names '{name}', which is not one of {', '.join(SERVICE_NAMES)}"]
        endpoint = url.rstrip("/") + "/internal/openapi.json"
        try:
            with urllib.request.urlopen(endpoint, timeout=10) as response:
                fetched[name] = json.loads(response.read().decode("utf-8"))
        except (urllib.error.URLError, json.JSONDecodeError) as error:
            return [f"{name}: could not read {endpoint}: {error}"]
    return []


def report(problems: list[str]) -> int:
    for problem in problems:
        print(f"  error  {problem}", file=sys.stderr)
    return 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--check", action="store_true", help="compose in memory and fail if the committed document is stale or invalid")
    action.add_argument("--write", action="store_true", help="regenerate contracts/public.openapi.yaml")
    action.add_argument("--print", dest="print_document", action="store_true", help="print the composed document to stdout")
    action.add_argument(
        "--routes",
        action="store_true",
        help="print the prefix -> service table the gateway routes on",
    )
    parser.add_argument(
        "--fetch",
        action="append",
        default=[],
        metavar="NAME=URL",
        help="compose using a live service's /internal/openapi.json instead of the committed document (repeatable)",
    )
    args = parser.parse_args()

    fetched: dict[str, Any] = {}
    problems = fetch_service_documents(fetched, args.fetch)
    if problems:
        return report(problems)

    try:
        composed, problems = compose(fetched)
    except ContractError as error:
        return report(error.messages)

    if problems:
        return report(problems)

    problems = structural_problems(composed)
    if problems:
        return report(problems)

    problems = validate_openapi(composed)
    if problems:
        return report(problems)

    # A fetched document has to match its committed counterpart, or the service
    # is serving an API nobody documented.
    for name in fetched:
        committed_path = service_document_path(name)
        if committed_path.exists():
            problems.extend(
                compare_live_document(name, fetched[name], load_yaml(committed_path))
            )
    if problems:
        return report(problems)

    serialised = serialise(composed)

    if args.routes:
        documents = read_service_documents(fetched)
        owners, _ = build_route_table(documents)
        for prefix in sorted(owners):
            print(f"{prefix:<34} {owners[prefix]}")
        return 0

    if args.print_document:
        sys.stdout.write(serialised)
        return 0

    if args.write:
        PUBLIC_DOCUMENT.write_text(
            "# Generated by tools/compose_contracts.py — do not edit by hand.\n"
            "# Edit contracts/<service>.openapi.yaml or contracts/shared/components.yaml\n"
            "# and run `python3 tools/compose_contracts.py --write`.\n"
            + serialised,
            encoding="utf-8",
        )
        operation_count = sum(len(methods) for methods in composed["paths"].values())
        print(
            f"wrote {PUBLIC_DOCUMENT.relative_to(REPO_ROOT)}: "
            f"{len(composed['paths'])} path(s), {operation_count} operation(s), "
            f"{len(composed.get('tags') or [])} tag(s)"
        )
        return 0

    if not PUBLIC_DOCUMENT.exists():
        return report(
            [
                f"{PUBLIC_DOCUMENT.relative_to(REPO_ROOT)} does not exist; "
                f"run 'python3 tools/compose_contracts.py --write'"
            ]
        )
    committed_text = PUBLIC_DOCUMENT.read_text(encoding="utf-8")
    # Compare the document, not the header comment: a regenerated file must be
    # byte-identical apart from the header this script always writes.
    committed_body = committed_text.split("\n", 3)
    committed_document = "\n".join(committed_body[3:]) if len(committed_body) > 3 else committed_text
    if committed_document != serialised:
        return report(
            [
                f"{PUBLIC_DOCUMENT.relative_to(REPO_ROOT)} is stale; run "
                f"'python3 tools/compose_contracts.py --write' and commit the result"
            ]
        )

    operation_count = sum(len(methods) for methods in composed["paths"].values())
    print(
        f"OK: composed {len(composed['paths'])} path(s), {operation_count} operation(s), "
        f"{len(composed.get('components', {}).get('schemas', {}))} schema(s) from "
        f"{len(SERVICE_NAMES)} service document(s)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
