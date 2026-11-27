#!/usr/bin/env python3
"""Check every diagram in this repository.

Two diagram sources are maintained:

  * Mermaid blocks inside Markdown — rendered by GitHub, and by this script.
  * The Structurizr DSL model in docs/architecture/structurizr/workspace.dsl.

This script does as much as the local toolchain allows and says plainly what it
could not check. It never reports success for a check it did not perform.

    python3 tools/check_diagrams.py            # report gaps, fail only on defects
    python3 tools/check_diagrams.py --strict   # CI gate: Mermaid must be rendered
    python3 tools/check_diagrams.py --full     # also require the DSL to be compiled
    MMDC=/path/to/mmdc python3 tools/check_diagrams.py

Exit status is non-zero if any check fails.

--strict fails when the Mermaid blocks could not be *rendered* (mmdc absent). Use
it in CI: "could not verify" must not look like "passed".

--full additionally fails when the Structurizr DSL could not be *compiled*, which
needs a Java runtime. It is separate from --strict because the Java gap is a known
property of this environment rather than of the artifacts, and a gate that is
permanently red is a gate everyone learns to ignore. The gap is still printed on
every run, and it is recorded in docs/architecture/structurizr/README.md.

Mermaid rendering needs @mermaid-js/mermaid-cli (`mmdc`). If it is missing, the
blocks are still extracted and syntax-checked, and the script reports that
rendering was skipped rather than claiming it passed.

Structurizr needs Java, which this host does not have. The DSL is therefore
checked structurally: brace balance, declared identifiers, and — the check that
matters most — that every step in a dynamic view corresponds to a relationship
declared in the model. That last one catches the common modelling error where a
flow diagram invents a connection the architecture does not have.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DSL = ROOT / "docs" / "architecture" / "structurizr" / "workspace.dsl"

DIAGRAM_KEYWORDS = {
    "flowchart", "graph", "sequenceDiagram", "classDiagram", "stateDiagram-v2",
    "stateDiagram", "erDiagram", "journey", "gantt", "pie", "quadrantChart",
    "requirementDiagram", "gitGraph", "mindmap", "timeline", "C4Context",
}

failures: list[str] = []
notes: list[tuple[str, bool]] = []  # (message, blocks --full only)


def fail(msg: str) -> None:
    failures.append(msg)


def markdown_files() -> list[Path]:
    out = []
    for p in sorted(ROOT.rglob("*.md")):
        if ".git" in p.parts or "node_modules" in p.parts:
            continue
        out.append(p)
    return out


# ---------------------------------------------------------------------------
# Mermaid
# ---------------------------------------------------------------------------

def extract_blocks(text: str) -> list[str]:
    return re.findall(r"```mermaid\n(.*?)```", text, re.S)


def check_mermaid() -> bool:
    """Returns True when blocks were rendered, False when only syntax-checked."""
    docs = markdown_files()
    blocks: list[tuple[str, int, str]] = []
    for d in docs:
        for i, b in enumerate(extract_blocks(d.read_text()), 1):
            rel = d.relative_to(ROOT)
            blocks.append((str(rel), i, b))

    print(f"Mermaid: {len(blocks)} block(s) in {len(docs)} markdown file(s)")

    # Unbalanced fences would silently break rendering on GitHub.
    for d in docs:
        text = d.read_text()
        if text.count("```") % 2 != 0:
            fail(f"{d.relative_to(ROOT)}: odd number of ``` fences")

    for source, index, body in blocks:
        first = next((ln.strip() for ln in body.splitlines() if ln.strip()), "")
        keyword = first.split()[0] if first else ""
        if keyword not in DIAGRAM_KEYWORDS:
            fail(f"{source} block {index}: unknown diagram type {keyword!r}")
        if not body.strip():
            fail(f"{source} block {index}: empty diagram")

    mmdc = os.environ.get("MMDC") or shutil.which("mmdc")
    if not mmdc:
        notes.append((
            "Mermaid blocks were syntax-checked but NOT rendered: mmdc not found. "
            "Install @mermaid-js/mermaid-cli to render.", False,
        ))
        print("  note  mmdc not found — syntax checked only, not rendered")
        return False

    puppeteer = os.environ.get("MMDC_PUPPETEER_CONFIG")
    rendered = 0
    with tempfile.TemporaryDirectory() as tmp:
        for source, index, body in blocks:
            name = f"{Path(source).stem}-{index}"
            src = Path(tmp) / f"{name}.mmd"
            out = Path(tmp) / f"{name}.svg"
            src.write_text(body)
            cmd = [mmdc, "-i", str(src), "-o", str(out), "--quiet"]
            if puppeteer:
                cmd += ["-p", puppeteer]
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=180)
            if r.returncode != 0 or not out.exists():
                detail = (r.stderr or r.stdout).strip().splitlines()
                fail(f"{source} block {index}: render failed — "
                     f"{detail[0][:160] if detail else 'no output'}")
            else:
                rendered += 1
    if rendered == len(blocks):
        print(f"  ok    rendered {len(blocks)} block(s)")
    else:
        print(f"  bad   rendered {rendered}/{len(blocks)} block(s)")
    return rendered == len(blocks)


# ---------------------------------------------------------------------------
# Structurizr
# ---------------------------------------------------------------------------

# Structurizr DSL element signatures: how many quoted strings each may take,
# not counting the identifier. Derived from the DSL grammar; a declaration with
# more strings than its type allows is a "too many tokens" error.
SIGNATURES = {
    #                    name  [desc] [tech] [instances] [tags]
    "person":             4,   # name, description, tags  (+ loc? no)
    "softwareSystem":     3,   # name, description, tags — NO technology field
    "container":          4,   # name, description, technology, tags
    "component":          4,   # name, description, technology, tags
    "deploymentNode":     5,   # name, description, technology, instances, tags
    "infrastructureNode": 4,   # name, description, technology, tags
    "deploymentEnvironment": 2,  # name, tags
}

DECL = re.compile(
    r"^\s*(?:(\w+)\s*=\s*)?(person|softwareSystem|container|component|"
    r"deploymentNode|infrastructureNode)\b(.*)$",
    re.M,
)

STRING = re.compile(r'"((?:[^"\\]|\\.)*)"')


def count_strings(rest: str) -> int:
    """Count quoted strings after the element type, ignoring any trailing '{'."""
    return len(STRING.findall(rest.split("{", 1)[0]))


def check_arity(text: str) -> None:
    """Catch the 'too many tokens' class: more strings than a type accepts.

    This is the error that shipped in the first version of workspace.dsl —
    a softwareSystem declared with a technology argument, which the parser
    reads as a fourth token and rejects. Brace counting could not see it.
    """
    for m in DECL.finditer(text):
        ident, kind, rest = m.group(1), m.group(2), m.group(3)
        allowed = SIGNATURES[kind]
        n = count_strings(rest)
        line = text[:m.start()].count("\n") + 1
        name = ident or "(anonymous)"
        if n > allowed:
            fail(f"workspace.dsl:{line}: {kind} {name!r} has {n} quoted argument(s), "
                 f"max {allowed} — the parser will report 'too many tokens'. "
                 f"Tags are a single comma-separated string, not separate arguments.")
            continue
        if kind != "person" and n < 1:
            fail(f"workspace.dsl:{line}: {kind} {name!r} has no name")
        # A softwareSystem or container with a technology-looking second string
        # is a common confusion; flag only the softwareSystem case, which is the
        # one the grammar forbids.
        if kind == "softwareSystem" and n == 3:
            args = STRING.findall(rest)
            third = args[2]
            if "," in third or " " in third and third[0].isupper() and len(third) < 40:
                notes.append((f"workspace.dsl:{line}: softwareSystem {name!r} has three "
                              f"arguments — the third is tags. If {third!r} was meant as "
                              f"technology, move it into the description.", True))


def run_cli(workspace: Path) -> bool:
    """Validate with the real Structurizr CLI when one is available.

    Returns True when the CLI ran and accepted the workspace. Brace counting and
    arity checking are heuristics; only the parser is authoritative.
    """
    cli = os.environ.get("STRUCTURIZR_CLI")
    if cli == "none":
        notes.append((
            "workspace.dsl was checked heuristically but NOT compiled: "
            "STRUCTURIZR_CLI=none.", True))
        return False
    if not cli:
        for cand in ("structurizr.sh", "structurizr", "/opt/structurizr/structurizr.sh"):
            found = shutil.which(cand) if not cand.startswith("/") else (
                cand if Path(cand).exists() else None)
            if found:
                cli = found
                break
    if not cli:
        notes.append((
            "workspace.dsl was checked heuristically but NOT compiled: no Java or no "
            "structurizr-cli. Set STRUCTURIZR_CLI, or paste the DSL into "
            "https://structurizr.com/dsl.", True))
        return False

    with tempfile.TemporaryDirectory() as tmp:
        r = subprocess.run(
            [cli, "validate", "-workspace", str(workspace)],
            capture_output=True, text=True, timeout=300, cwd=tmp,
        )
    out = (r.stdout + r.stderr).strip()
    if r.returncode != 0 or "Error" in out or "error" in out:
        for line in out.splitlines():
            if line.strip():
                fail(f"structurizr-cli: {line.strip()[:200]}")
        return False
    print("  ok    structurizr-cli compiled the workspace")
    return True


def check_structurizr() -> bool:
    if not DSL.exists():
        fail(f"{DSL.relative_to(ROOT)}: missing")
        return False

    text = DSL.read_text()
    print(f"Structurizr: {DSL.relative_to(ROOT)}")

    depth = 0
    for lineno, line in enumerate(text.splitlines(), 1):
        code = line.split("//", 1)[0]
        depth += code.count("{") - code.count("}")
        if depth < 0:
            fail(f"workspace.dsl:{lineno}: unbalanced closing brace")
            return False
    if depth != 0:
        fail(f"workspace.dsl: {depth} unclosed brace(s)")
        return False
    print("  ok    braces balanced")

    model_part, _, views_part = text.partition("views {")

    declared = {m.group(1) for m in re.finditer(
        r"^\s*(\w+)\s*=\s*(?:person|softwareSystem|container|component|"
        r"deploymentNode|infrastructureNode)\b", model_part, re.M)}
    print(f"  ok    {len(declared)} element(s) declared")

    check_arity(text)

    instances = set(re.findall(r"^\s*containerInstance\s+(\w+)\s*$", text, re.M))
    unknown = instances - declared
    if unknown:
        fail(f"workspace.dsl: containerInstance of undeclared element(s): "
             f"{', '.join(sorted(unknown))}")

    rel = re.compile(r"^\s*(\w+)\s*->\s*(\w+)\s", re.M)
    model_rels = {(m.group(1), m.group(2)) for m in rel.finditer(model_part)}
    for src, dst in sorted(model_rels):
        for side, name in (("source", src), ("target", dst)):
            if name not in declared:
                fail(f"workspace.dsl: relationship {src} -> {dst} uses undeclared "
                     f"{side} {name!r}")
    print(f"  ok    {len(model_rels)} relationship(s) declared")

    for view_name, body in re.findall(
            r"dynamic\s+\w+\s+\"([^\"]+)\".*?\{(.*?)\n\s*\}", views_part, re.S):
        steps = [(m.group(1), m.group(2)) for m in rel.finditer(body)]
        for src, dst in steps:
            if (src, dst) not in model_rels:
                fail(f"workspace.dsl: dynamic view {view_name!r} steps "
                     f"{src} -> {dst}, which is not a declared relationship")
        if steps:
            print(f"  ok    dynamic view {view_name!r}: {len(steps)} step(s) all declared")

    return run_cli(DSL)


# ---------------------------------------------------------------------------

def main() -> int:
    strict = "--strict" in sys.argv[1:] or "--full" in sys.argv[1:]
    full = "--full" in sys.argv[1:]
    rendered = check_mermaid()
    compiled = check_structurizr()

    print()
    if notes:
        print("Not verified:")
        for msg, full_only in notes:
            tag = "  (blocks --full only)" if full_only else ""
            print(f"  - {msg}{tag}")
        print()

    if failures:
        print(f"FAILED: {len(failures)} problem(s)")
        for f in failures:
            print(f"  - {f}")
        return 1

    blocking = [m for m, full_only in notes if full or not full_only]
    if strict and blocking:
        flag = "--full" if full else "--strict"
        print(f"FAILED ({flag}): {len(blocking)} check(s) could not be performed")
        for m in blocking:
            print(f"  - {m}")
        return 1

    mermaid_scope = "rendered" if rendered else "syntax-checked only"
    dsl_scope = "compiled" if compiled else "checked heuristically"
    print(f"OK: Mermaid {mermaid_scope}; Structurizr DSL {dsl_scope}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
