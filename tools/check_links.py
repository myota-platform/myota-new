#!/usr/bin/env python3
"""Check every relative link in the repository resolves.

    python3 tools/check_links.py

Checks, across all Markdown:

  - relative links and image sources point at a path that exists
  - in-page anchors (`#fragment`) match a heading in the target file
  - HTML `src`/`href` inside the UI pages resolve to a real file

External links are not fetched: a network check would fail on a flaky day and
teach everyone to ignore the result. Broken external links are a different tool's
job.

This exists because link drift is the cheapest documentation bug to introduce and
the easiest to miss. It has already caught a document placed one directory too
high, whose links were all relative to the wrong place.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
SKIP_DIRS = {".git", "node_modules", "target", "dist", "build", "coverage"}

MD_LINK = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)")
HTML_ATTR = re.compile(r'(?:src|href)="([^"]+)"')


def walk(suffixes: set[str]) -> list[Path]:
    out = []
    for p in ROOT.rglob("*"):
        if not p.is_file() or p.suffix not in suffixes:
            continue
        if any(part in SKIP_DIRS for part in p.relative_to(ROOT).parts):
            continue
        out.append(p)
    return sorted(out)


def headings(path: Path) -> set[str]:
    """GitHub-style slugs for the headings in a Markdown file."""
    text = path.read_text(errors="ignore")
    slugs = set()
    for line in text.splitlines():
        m = re.match(r"^#{1,6}\s+(.*?)\s*$", line)
        if not m:
            continue
        title = m.group(1)
        # GitHub's algorithm: lowercase, drop characters that are not word
        # characters, spaces or hyphens, then replace *each* space with a hyphen.
        # Spaces are not collapsed — "## 01 · Editorial" becomes "01--editorial",
        # because the middle dot is removed and leaves its two flanking spaces.
        slug = re.sub(r"[^\w\s-]", "", title.lower())
        slug = slug.replace(" ", "-")
        slugs.add(slug)
        # headings with inline code or emphasis still slugify the same way
        slugs.add(re.sub(r"[`*_]", "", slug))
    return slugs


def main() -> int:
    problems: list[str] = []
    checked = 0

    for path in walk({".md"}):
        text = path.read_text(errors="ignore")
        for m in MD_LINK.finditer(text):
            target = m.group(1)
            if target.startswith(("http://", "https://", "mailto:", "tel:")):
                continue

            checked += 1
            file_part, _, fragment = target.partition("#")
            resolved = (path.parent / unquote(file_part)).resolve() if file_part else path

            if file_part and not resolved.exists():
                problems.append(f"{path.relative_to(ROOT)}: {target} — no such file")
                continue

            if fragment and resolved.suffix == ".md" and resolved.exists():
                if fragment not in headings(resolved):
                    problems.append(
                        f"{path.relative_to(ROOT)}: {target} — no heading matching #{fragment}"
                    )

    for path in walk({".html"}):
        text = path.read_text(errors="ignore")
        for m in HTML_ATTR.finditer(text):
            target = m.group(1)
            if target.startswith(("http://", "https://", "mailto:", "#", "data:")):
                continue
            checked += 1
            resolved = (path.parent / unquote(target.split("#")[0])).resolve()
            if target.split("#")[0] and not resolved.exists():
                problems.append(f"{path.relative_to(ROOT)}: {target} — no such file")

    print(f"checked {checked} relative link(s)")

    if problems:
        print(f"\nFAILED: {len(problems)} broken")
        for p in problems:
            print(f"  - {p}")
        return 1

    print("OK: every relative link and anchor resolves")
    return 0


if __name__ == "__main__":
    sys.exit(main())
