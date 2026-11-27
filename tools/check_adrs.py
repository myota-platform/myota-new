#!/usr/bin/env python3
"""Check the decision records.

    python3 tools/check_adrs.py

Verifies, for every ADR in docs/decisions/:

  - numbering is sequential from 0001 with no gaps or duplicates
  - each file has the required sections (Context, Decision, Alternatives,
    Consequences, Verification)
  - each has a Status line using the documented vocabulary, and a Provenance line
  - an accepted ADR is referenced from AGENTS.md, because an accepted decision
    that is not encoded as a rule is a decision nobody will follow
  - a proposed ADR is *not* required in AGENTS.md, but if it is referenced the
    reference must not claim it is in force

Exit status is non-zero on any failure.

This exists because "is every decision properly recorded?" is otherwise answered
from memory. The gaps it was written to catch were real: ADR-0002 was marked
accepted when the brief only asked for suggestions, and the UI decision had a rule
in AGENTS.md but no ADR at all.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DECISIONS = ROOT / "docs" / "decisions"
AGENTS = ROOT / "AGENTS.md"

REQUIRED_SECTIONS = ["Context", "Decision", "Alternatives", "Consequences", "Verification"]
VALID_STATUS = {"proposed", "accepted", "superseded", "rejected"}

failures: list[str] = []


def fail(msg: str) -> None:
    failures.append(msg)


def adr_files() -> list[Path]:
    return sorted(p for p in DECISIONS.glob("[0-9][0-9][0-9][0-9]-*.md"))


def check_numbering(files: list[Path]) -> None:
    numbers = [int(p.name[:4]) for p in files]
    if not numbers:
        fail("no ADR files found")
        return
    expected = list(range(1, len(numbers) + 1))
    if numbers != expected:
        fail(f"ADR numbering is not sequential from 0001: found {numbers}, expected {expected}")


def check_structure(path: Path) -> dict[str, str]:
    text = path.read_text()
    name = path.name

    meta: dict[str, str] = {}
    for key in ("Status", "Provenance", "Supersedes"):
        m = re.search(rf"^\*\*{key}:\*\*\s*(.+)$", text, re.M)
        if m:
            meta[key] = m.group(1).strip()

    if "Status" not in meta:
        fail(f"{name}: no Status line")
    else:
        status = meta["Status"].split("·")[0].strip().lower()
        if status not in VALID_STATUS:
            fail(f"{name}: status {status!r} is not one of {sorted(VALID_STATUS)}")
        if "·" not in meta["Status"]:
            fail(f"{name}: Status has no date (expected 'status · YYYY-MM-DD')")

    if "Provenance" not in meta:
        fail(f"{name}: no Provenance line — 'accepted' is a claim and needs a source")

    headings = {m.group(1).strip() for m in re.finditer(r"^##\s+(.+)$", text, re.M)}
    for section in REQUIRED_SECTIONS:
        if not any(h.startswith(section) for h in headings):
            fail(f"{name}: missing the {section} section")

    # a decision record with no costs is advocacy, not a record
    # a cost heading may be qualified ("**Cost, if polyglot**"), so match the
    # prefix rather than requiring the label to be bare
    cost = re.search(r"^\*\*Cost\b", text, re.M)
    if "Consequences" in headings and not cost:
        fail(f"{name}: Consequences has no **Cost** subsection")

    return meta


def check_agents_references(meta_by_file: dict[str, dict[str, str]]) -> None:
    agents = AGENTS.read_text()

    for name, meta in meta_by_file.items():
        number = name[:4]
        status = meta.get("Status", "").split("·")[0].strip().lower()
        referenced = f"ADR-{number}" in agents

        if status == "accepted" and not referenced:
            fail(f"{name}: accepted but not referenced from AGENTS.md — "
                 f"an accepted decision that is not a rule is a decision nobody follows")
        if status == "proposed" and referenced:
            # allowed, but the reference must not read as settled
            idx = agents.index(f"ADR-{number}")
            window = agents[max(0, idx - 200): idx + 200].lower()
            if "proposed" not in window:
                fail(f"{name}: proposed, but the AGENTS.md reference does not say so")


def check_mapping_table() -> None:
    """The ADR → AGENTS.md rule table must point at headings that exist.

    Renumbering AGENTS.md is the easy mistake: the rules stay correct while the
    table quietly points §6 at what is now §7. Verifying the section number exists
    *and* that its heading contains the claimed topic catches that drift, which a
    plain "is this section number valid" check would miss.
    """
    readme = DECISIONS / "README.md"
    if not readme.exists():
        return
    text = readme.read_text()

    if "| ADR | AGENTS.md rule |" not in text:
        fail("docs/decisions/README.md has no ADR-to-rule mapping table")
        return

    headings = {}
    for m in re.finditer(r"^## (\d+)\. (.+)$", AGENTS.read_text(), re.M):
        headings[int(m.group(1))] = m.group(2)

    rows = re.findall(r"^\| (\d{4}) \| §(\d+)\s+([^|]+?)\s*\|?$", text, re.M)
    if not rows:
        fail("the ADR-to-rule mapping table has no parseable rows")
        return

    for adr, number, claim in rows:
        number = int(number)
        claim = claim.replace("—", "-").strip()
        if number not in headings:
            fail(f"mapping table: ADR-{adr} points at §{number}, which does not exist")
            continue
        heading = headings[number]
        # the first few words of the claim must appear in the heading
        key = claim.split("-")[0].strip().split()[0].lower()
        if key not in heading.lower():
            fail(f"mapping table: ADR-{adr} says §{number} is {claim!r}, "
                 f"but §{number} is {heading!r} — the table is stale")


def check_readme_lists(files: list[Path]) -> None:
    readme = DECISIONS / "README.md"
    if not readme.exists():
        fail("docs/decisions/README.md is missing — there is no documented ADR process")
        return
    text = readme.read_text()
    for path in files:
        if path.name not in text:
            fail(f"{path.name}: not listed in docs/decisions/README.md")


def main() -> int:
    files = adr_files()
    check_numbering(files)

    meta_by_file = {}
    for path in files:
        meta_by_file[path.name] = check_structure(path)

    check_agents_references(meta_by_file)
    check_mapping_table()
    check_readme_lists(files)

    print(f"checked {len(files)} decision record(s)")
    for path in files:
        status = meta_by_file[path.name].get("Status", "?").split("·")[0].strip()
        print(f"  {path.name:52s} {status}")

    if failures:
        print(f"\nFAILED: {len(failures)} problem(s)")
        for f in failures:
            print(f"  - {f}")
        return 1

    print("\nOK: all ADRs are numbered, complete, sourced, and reflected in AGENTS.md")
    return 0


if __name__ == "__main__":
    sys.exit(main())
