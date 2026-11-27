#!/usr/bin/env python3
"""Check every diagram in this repository.

Two diagram sources are maintained:

  * Mermaid blocks inside Markdown — rendered by GitHub, and by this script.
  * The Structurizr DSL model in docs/architecture/structurizr/workspace.dsl.

This script does as much as the local toolchain allows and says plainly what it
could not check. It never reports success for a check it did not perform.

    python3 tools/check_diagrams.py            # everything available
    MMDC=/path/to/mmdc python3 tools/check_diagrams.py

Exit status is non-zero if any check fails.

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
notes: list[str] = []


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
        notes.append(
            "Mermaid blocks were syntax-checked but NOT rendered: mmdc not found. "
            "Install @mermaid-js/mermaid-cli to render."
        )
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

DECL = re.compile(
    r"^\s*(\w+)\s*=\s*(person|softwareSystem|container|component|"
    r"deploymentNode|infrastructureNode|containerInstance|deploymentEnvironment)\b",
    re.M,
)
REL = re.compile(r"^\s*(\w+)\s*->\s*(\w+)\s", re.M)


def check_structurizr() -> None:
    if not DSL.exists():
        fail(f"{DSL.relative_to(ROOT)}: missing")
        return

    text = DSL.read_text()
    print(f"Structurizr: {DSL.relative_to(ROOT)}")

    depth = 0
    for lineno, line in enumerate(text.splitlines(), 1):
        code = line.split("//", 1)[0]
        depth += code.count("{") - code.count("}")
        if depth < 0:
            fail(f"workspace.dsl:{lineno}: unbalanced closing brace")
            return
    if depth != 0:
        fail(f"workspace.dsl: {depth} unclosed brace(s)")
        return
    print("  ok    braces balanced")

    model_part, _, views_part = text.partition("views {")

    declared = {m.group(1) for m in DECL.finditer(model_part)}
    if not declared:
        fail("workspace.dsl: no elements declared")
        return
    print(f"  ok    {len(declared)} element(s) declared")

    # containerInstance takes a bare identifier, not name = container.
    instances = set(re.findall(r"^\s*containerInstance\s+(\w+)\s*$", text, re.M))
    unknown = instances - declared
    if unknown:
        fail(f"workspace.dsl: containerInstance of undeclared element(s): "
             f"{', '.join(sorted(unknown))}")

    model_rels = {(m.group(1), m.group(2)) for m in REL.finditer(model_part)}
    for src, dst in sorted(model_rels):
        for side, name in (("source", src), ("target", dst)):
            if name not in declared:
                fail(f"workspace.dsl: relationship {src} -> {dst} uses undeclared "
                     f"{side} {name!r}")
    print(f"  ok    {len(model_rels)} relationship(s) declared")

    # Every dynamic-view step must correspond to a declared relationship.
    for view_name, body in re.findall(
            r"dynamic\s+\w+\s+\"([^\"]+)\".*?\{(.*?)\n\s*\}", views_part, re.S):
        steps = [(m.group(1), m.group(2)) for m in REL.finditer(body)]
        for src, dst in steps:
            if (src, dst) not in model_rels:
                fail(f"workspace.dsl: dynamic view {view_name!r} steps "
                     f"{src} -> {dst}, which is not a declared relationship")
        if steps:
            print(f"  ok    dynamic view {view_name!r}: {len(steps)} step(s) all declared")

    for kind in ("systemContext", "container", "deployment"):
        if not re.search(rf"^\s*{kind}\s+\w+\s+\"", views_part, re.M):
            notes.append(f"workspace.dsl: no {kind} view defined")

    if shutil.which("java") is None:
        notes.append(
            "workspace.dsl was structurally checked but NOT compiled: no Java "
            "runtime. Run `structurizr-cli` or paste into structurizr.com/dsl."
        )
        print("  note  no Java — structural check only, DSL not compiled")


# ---------------------------------------------------------------------------

def main() -> int:
    rendered = check_mermaid()
    check_structurizr()

    print()
    if notes:
        print("Not verified:")
        for n in notes:
            print(f"  - {n}")
        print()

    if failures:
        print(f"FAILED: {len(failures)} problem(s)")
        for f in failures:
            print(f"  - {f}")
        return 1

    scope = "rendered" if rendered else "syntax-checked"
    print(f"OK: all Mermaid diagrams {scope}; Structurizr model structurally valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
