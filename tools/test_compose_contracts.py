#!/usr/bin/env python3
"""Tests for the contract composer.

Not a test framework: a script with `assert`s, run by `make checks-contracts` and
by CI. The repository has no Python test runner and adding one to test one script
would be heavier than the thing under test.

Every case is a document mutated in memory and re-composed, so a failure names the
rule that broke rather than a line in a YAML file. Two cases need a file, and use a
temporary directory.

Run:
    python3 tools/test_compose_contracts.py
"""

from __future__ import annotations

import copy
import sys
import tempfile
from pathlib import Path
from typing import Any, Callable

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compose_contracts as composer

FAILURES: list[str] = []
CASES = 0


def service_documents() -> dict[str, Any]:
    return {
        name: copy.deepcopy(composer.load_yaml(composer.service_document_path(name)))
        for name in composer.SERVICE_NAMES
    }


def first_path(document: dict, prefix: str) -> str:
    for path in document["paths"]:
        if path.startswith(prefix):
            return path
    raise AssertionError(f"no path starting with {prefix}")


def first_operation(document: dict, path: str) -> dict:
    path_item = document["paths"][path]
    for method in ("get", "post", "patch", "put", "delete"):
        if method in path_item:
            return path_item[method]
    raise AssertionError(f"{path} has no operations")


def expect_problem(name: str, mutate: Callable[[dict], None], pattern: str) -> None:
    global CASES
    CASES += 1
    documents = service_documents()
    mutate(documents)
    try:
        _, problems = composer.compose(documents)
    except composer.ContractError as error:  # a rule the loader enforces
        problems = error.messages
    if not problems:
        FAILURES.append(f"{name}: composed without a problem, expected one")
        print(f"  FAIL  {name}")
        return
    if not any(pattern in problem for problem in problems):
        FAILURES.append(f"{name}: no problem contained {pattern!r}; got {problems[:2]}")
        print(f"  FAIL  {name}")
        return
    print(f"  ok    {name}")


def expect_clean(name: str, documents: dict[str, Any]) -> None:
    global CASES
    CASES += 1
    _, problems = composer.compose(documents)
    if problems:
        FAILURES.append(f"{name}: expected no problems, got {problems[:3]}")
        print(f"  FAIL  {name}")
        return
    print(f"  ok    {name}")


# ── mutations ────────────────────────────────────────────────────────────────


def duplicate_operation_id(documents: dict) -> None:
    path = first_path(documents["geodata"], "/v1/entities/")
    first_operation(documents["geodata"], path)["operationId"] = "core.me.read"


def path_owned_twice(documents: dict) -> None:
    path = first_path(documents["core"], "/v1/me")
    documents["geodata"]["paths"][path] = copy.deepcopy(documents["core"]["paths"][path])


def prefix_owned_twice(documents: dict) -> None:
    documents["geodata"]["paths"]["/v1/auth/enrol"] = copy.deepcopy(
        documents["core"]["paths"]["/v1/auth/logout"]
    )
    documents["geodata"]["paths"]["/v1/auth/enrol"]["post"]["operationId"] = "geodata.auth.enrol"


def tag_owned_twice(documents: dict) -> None:
    documents["geodata"]["tags"].append({"name": "me", "description": "collides with core"})


def internal_on_a_published_path(documents: dict) -> None:
    operation = first_operation(documents["core"], "/v1/me")
    operation["x-internal"] = True


def published_path_under_internal(documents: dict) -> None:
    documents["core"]["paths"]["/internal/openapi.json"]["get"].pop("x-internal")


def post_without_idempotency_key(documents: dict) -> None:
    operation = documents["activity"]["paths"]["/v1/activations"]["post"]
    operation["parameters"] = [
        parameter
        for parameter in operation.get("parameters") or []
        if "IdempotencyKey" not in (parameter.get("$ref") or "")
    ]


def exemption_without_reason(documents: dict) -> None:
    operation = documents["activity"]["paths"]["/v1/activations"]["post"]
    operation["parameters"] = []
    operation["x-idempotency"] = "not-applicable"


def bare_array_response(documents: dict) -> None:
    operation = first_operation(documents["geodata"], "/v1/entities")
    operation["responses"]["200"]["content"]["application/json"]["schema"] = {
        "type": "array",
        "items": {"type": "string"},
    }


def dangling_shared_reference(documents: dict) -> None:
    """Point one shared reference at a component that does not exist."""
    for path_item in documents["core"]["paths"].values():
        for operation in path_item.values():
            for response in (operation.get("responses") or {}).values():
                reference = response.get("$ref") if isinstance(response, dict) else None
                if reference and reference.endswith("ProblemNotFound"):
                    response["$ref"] = reference.replace("ProblemNotFound", "ProblemGone")
                    return
    raise AssertionError("no ProblemNotFound response to mutate")


def foreign_reference(documents: dict) -> None:
    operation = documents["core"]["paths"]["/v1/me"]["patch"]
    operation["requestBody"]["content"]["application/json"]["schema"] = {
        "$ref": "./elsewhere.yaml#/components/schemas/ProfileUpdate"
    }


def operation_without_tag(documents: dict) -> None:
    first_operation(documents["core"], "/v1/me").pop("tags")


def operation_without_summary(documents: dict) -> None:
    first_operation(documents["core"], "/v1/me").pop("summary")


def operation_without_example(documents: dict) -> None:
    operation = first_operation(documents["core"], "/v1/me")
    operation["responses"]["200"]["content"]["application/json"].pop("example")


def undeclared_tag(documents: dict) -> None:
    first_operation(documents["core"], "/v1/me")["tags"] = ["not-a-tag"]


def error_as_json_not_problem(documents: dict) -> None:
    operation = first_operation(documents["core"], "/v1/me")
    operation["responses"]["400"] = {
        "description": "wrong media type",
        "content": {"application/json": {"schema": {"type": "object"}}},
    }


# ── the cases ────────────────────────────────────────────────────────────────

print("composer tests")

expect_clean("the committed documents compose cleanly", service_documents())

expect_problem("duplicate operationId", duplicate_operation_id, "duplicate operationId")
expect_problem("path owned by two services", path_owned_twice, "declared by both")
expect_problem("prefix owned by two services", prefix_owned_twice, "is claimed by")
expect_problem("tag declared by two services", tag_owned_twice, "is declared by both")
expect_problem("internal operation on a public path", internal_on_a_published_path, "marked internal but is not under")
expect_problem("published operation under /internal", published_path_under_internal, "not marked x-internal")
expect_problem("POST without Idempotency-Key", post_without_idempotency_key, "does not document Idempotency-Key")
expect_problem("exemption without a reason", exemption_without_reason, "gives no reason")
expect_problem("bare array response", bare_array_response, "returns a bare array")
expect_problem("dangling shared reference", dangling_shared_reference, "dangling shared reference")
expect_problem("reference outside the contract set", foreign_reference, "may only reference itself")
expect_problem("operation without a tag", operation_without_tag, "has no tag")
expect_problem("operation without a summary", operation_without_summary, "has no summary")
expect_problem("operation without an example", operation_without_example, "has no example")
expect_problem("undeclared tag", undeclared_tag, "uses undeclared tag")
expect_problem("error response that is not problem+json", error_as_json_not_problem, "errors are application/problem+json")


# ── cases that need a file ───────────────────────────────────────────────────


def case_duplicate_yaml_key() -> None:
    global CASES
    CASES += 1
    with tempfile.TemporaryDirectory() as directory:
        document = Path(directory) / "dupe.yaml"
        document.write_text(
            "openapi: 3.1.0\npaths:\n  /v1/a:\n    get: {}\n  /v1/a:\n    post: {}\n",
            encoding="utf-8",
        )
        try:
            composer.load_yaml(document)
        except composer.ContractError as error:
            if "duplicate key" in error.messages[0]:
                print("  ok    duplicate key in a YAML mapping")
                return
            FAILURES.append(f"duplicate key: wrong error {error.messages[0]}")
            print("  FAIL  duplicate key in a YAML mapping")
            return
    FAILURES.append("duplicate key: loaded without an error")
    print("  FAIL  duplicate key in a YAML mapping")


def case_missing_document() -> None:
    global CASES
    CASES += 1
    documents = service_documents()
    documents.pop("core")
    original = composer.service_document_path
    composer.service_document_path = lambda name: Path("/nonexistent") / f"{name}.openapi.yaml"
    try:
        composer.compose(documents)
    except composer.ContractError as error:
        if any("does not exist" in message for message in error.messages):
            print("  ok    missing service document")
            CASES += 0
            return
        FAILURES.append(f"missing document: wrong error {error.messages[0]}")
        print("  FAIL  missing service document")
        return
    finally:
        composer.service_document_path = original
    FAILURES.append("missing document: composed without an error")
    print("  FAIL  missing service document")


def case_committed_document_is_current() -> None:
    global CASES
    CASES += 1
    composed, problems = composer.compose({})
    if problems:
        FAILURES.append(f"staleness check: composition reported {problems[:2]}")
        print("  FAIL  committed composed document is current")
        return
    serialised = composer.serialise(composed)
    committed = composer.PUBLIC_DOCUMENT.read_text(encoding="utf-8")
    body = "\n".join(committed.split("\n", 3)[3:]) if committed.count("\n") >= 3 else committed
    if body != serialised:
        FAILURES.append("staleness check: public.openapi.yaml differs from a fresh composition")
        print("  FAIL  committed composed document is current")
        return
    print("  ok    committed composed document is current")


case_duplicate_yaml_key()
case_missing_document()
case_committed_document_is_current()

print()
if FAILURES:
    for failure in FAILURES:
        print(f"  error  {failure}", file=sys.stderr)
    print(f"{len(FAILURES)} of {CASES} composer test(s) failed", file=sys.stderr)
    sys.exit(1)

print(f"OK: {CASES} composer test(s) passed")
