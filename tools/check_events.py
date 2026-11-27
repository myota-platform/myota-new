#!/usr/bin/env python3
"""Check the event catalogue against the documents that define it.

There are three places an event type can be named, and they drift silently:

  1. `docs/architecture/messaging.md` — the consumer/source table, the stated
     source of truth for which events exist.
  2. `contracts/events/index.json` — the catalogue the schemas are found by.
  3. `contracts/events/<type>.json` — the schema itself, whose `type` constant is
     the only one that constrains the wire.

A type added to the code and not to the documents is an event no consumer can
find a schema for. This script makes that a failing check rather than a discovery
during an incident.

Checks:
  - every event type in the messaging table has a schema file
  - every schema file is listed in index.json, and every listed file exists
  - each file's `type` const equals its filename stem, and equals an
    `allOf.objects.properties.type.const` the file actually declares
  - each schema's `source` const matches the type's service prefix
  - each file documents at least one example, and that example validates against
    its own schema when `jsonschema` is installed
  - every type is mentioned somewhere under docs/architecture/

The validation is skipped with a note when `jsonschema` is absent, for the same
reason `check_diagrams.py` reports a missing `mmdc`: a check that cannot run must
say so.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
EVENTS_DIR = REPO_ROOT / "contracts" / "events"
INDEX = EVENTS_DIR / "index.json"
ENVELOPE = EVENTS_DIR / "v1.schema.json"
MESSAGING_DOC = REPO_ROOT / "docs" / "architecture" / "messaging.md"
ARCHITECTURE_DIR = REPO_ROOT / "docs" / "architecture"

# A row of the "What the consumers subscribe to" table:
#   | `activity` | `core` | `account.created`, `account.closed` |
CONSUMER_ROW = re.compile(r"^\|\s*`([a-z]+)`\s*\|\s*`([a-z]+)`\s*\|(.+)\|\s*$")
BACKTICKED = re.compile(r"`([a-z0-9_]+(?:\.[a-z0-9_]+)+)`")
TYPE_PATTERN = re.compile(r"^([a-z]+)\.([a-z0-9_]+(?:\.[a-z0-9_]+)*)\.v[0-9]+$")


def documented_types() -> dict[str, str]:
    """Event types named in the messaging table, mapped to the row that names them."""
    if not MESSAGING_DOC.exists():
        return {}
    found: dict[str, str] = {}
    for line in MESSAGING_DOC.read_text(encoding="utf-8").splitlines():
        row = CONSUMER_ROW.match(line)
        if not row:
            continue
        consumer, source, types_cell = row.groups()
        for listed in BACKTICKED.findall(types_cell):
            # The table lists `<aggregate>.<fact>` against the producing service;
            # the wire type prefixes the service and suffixes the version.
            found[f"{source}.{listed}.v1"] = f"{consumer} consumes from {source}"
    return found


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def find_property(schema: dict, name: str) -> dict | None:
    """The first `properties.<name>` node found anywhere in a schema."""
    stack: list[Any] = [schema]
    while stack:
        node = stack.pop()
        if isinstance(node, dict):
            properties = node.get("properties")
            if isinstance(properties, dict) and isinstance(properties.get(name), dict):
                return properties[name]
            stack.extend(node.values())
        elif isinstance(node, list):
            stack.extend(node)
    return None


def declared_constants(schema: dict) -> dict[str, str]:
    """The `properties.<key>.const` values a schema declares at any depth."""
    constants: dict[str, str] = {}

    def walk(node: Any) -> None:
        if isinstance(node, dict):
            properties = node.get("properties")
            if isinstance(properties, dict):
                for key, value in properties.items():
                    if isinstance(value, dict) and "const" in value:
                        constants.setdefault(key, value["const"])
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for item in node:
                walk(item)

    walk(schema)
    return constants


def inline_envelope(schema: dict, envelope: dict) -> dict:
    """Replace the `v1.schema.json` reference with the envelope itself.

    A resolver would need a registry of retrieval URIs; substituting is
    equivalent for a two-document graph and does not pretend to be a resolver.
    """
    if isinstance(schema, dict):
        if schema.get("$ref") == "v1.schema.json":
            return envelope
        return {key: inline_envelope(value, envelope) for key, value in schema.items()}
    if isinstance(schema, list):
        return [inline_envelope(item, envelope) for item in schema]
    return schema


def validate_example(schema: dict, example: Any) -> list[str]:
    try:
        import jsonschema
    except ImportError:
        return []
    validator = jsonschema.Draft202012Validator(inline_envelope(schema, load_json(ENVELOPE)))
    return [
        f"{error.json_path}: {error.message}"
        for error in sorted(validator.iter_errors(example), key=lambda e: list(e.path))
    ]


def main() -> int:
    problems: list[str] = []

    for required in (INDEX, ENVELOPE):
        if not required.exists():
            problems.append(f"{required.relative_to(REPO_ROOT)} does not exist")
    if problems:
        for problem in problems:
            print(f"  error  {problem}", file=sys.stderr)
        return 1

    envelope = load_json(ENVELOPE)
    index = load_json(INDEX).get("events") or {}
    schema_files = sorted(
        path for path in EVENTS_DIR.glob("*.json") if path.name not in {"index.json"}
    )
    schema_by_type = {path.stem: path for path in schema_files}

    # 1. Everything the messaging document names has a schema.
    for event_type, provenance in sorted(documented_types().items()):
        if event_type not in schema_by_type:
            problems.append(
                f"{event_type} is listed in messaging.md ({provenance}) but has no schema"
            )

    # 2. The index and the directory agree.
    for event_type, filename in sorted(index.items()):
        if not (EVENTS_DIR / filename).exists():
            problems.append(f"index.json lists {event_type} -> {filename}, which does not exist")
    for event_type, path in sorted(schema_by_type.items()):
        if event_type == "v1" + ".schema":
            continue
        if event_type not in index:
            problems.append(f"{path.name} exists but is not listed in index.json")

    architecture_text = "\n".join(
        doc.read_text(encoding="utf-8") for doc in sorted(ARCHITECTURE_DIR.glob("*.md"))
    )

    checked = 0
    for event_type, path in sorted(schema_by_type.items()):
        if path.name == ENVELOPE.name:
            continue
        checked += 1
        schema = load_json(path)
        constants = declared_constants(schema)

        if constants.get("type") != event_type:
            problems.append(
                f"{path.name}: type const is {constants.get('type')!r}, expected {event_type!r}"
            )

        match = TYPE_PATTERN.match(event_type)
        if not match:
            problems.append(f"{path.name}: '{event_type}' is not a valid event type name")
            continue
        service = match.group(1)
        # `source` is the producer. It is a constant for every event with one
        # producer, and a pattern for the one event any service may raise —
        # `core.notification.requested.v1`, whose type namespace names the domain
        # owner, not the publisher.
        source_schema = find_property(schema, "source") or {}
        if "const" in source_schema:
            if source_schema["const"] != f"/{service}":
                problems.append(
                    f"{path.name}: source const is {source_schema['const']!r}, "
                    f"expected '/{service}'"
                )
        elif "pattern" not in source_schema:
            problems.append(
                f"{path.name}: does not constrain 'source' to a service, so a "
                f"producer could publish under any name"
            )

        if event_type not in architecture_text:
            problems.append(
                f"{event_type} has a schema but is not mentioned in docs/architecture/"
            )

        examples = schema.get("examples") or []
        if not examples:
            problems.append(f"{path.name}: no example, so nothing exercises the schema")
            continue
        for number, example in enumerate(examples, start=1):
            for failure in validate_example(schema, example):
                problems.append(f"{path.name}: example {number} fails its own schema at {failure}")

    # 3. The envelope has to be a CloudEvents 1.0 subset: the four required
    #    attributes must be required here too, or a consumer cannot route on them.
    required_attributes = set(envelope.get("required") or [])
    for attribute in ("id", "source", "specversion", "type"):
        if attribute not in required_attributes:
            problems.append(f"v1.schema.json does not require the CloudEvents attribute '{attribute}'")
    if (envelope.get("properties", {}).get("specversion") or {}).get("const") != "1.0":
        problems.append("v1.schema.json does not pin specversion to '1.0'")

    if problems:
        for problem in problems:
            print(f"  error  {problem}", file=sys.stderr)
        return 1

    try:
        import jsonschema  # noqa: F401
    except ImportError:
        print(
            "  note  jsonschema not installed — examples were not validated.\n"
            "        pip install -r tools/requirements.txt",
            file=sys.stderr,
        )

    print(f"OK: {checked} event schema(s) match messaging.md and their own examples")
    return 0


if __name__ == "__main__":
    sys.exit(main())
