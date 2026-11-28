#!/usr/bin/env python3
"""Check that a front end conforms to the shared design system.

    python3 tools/check_design_system.py web/admin

Two assertions, both cheap and both about drift:

1. **The app imports the shared token file** rather than carrying its own palette.
   `libs/ui-tokens/tokens.css` is the one place `--clay`, `--sage` and friends are
   defined. A second copy is how two screens stop looking like one product.

2. **The app does not redefine the palette.** Importing the tokens and then
   overriding `--clay` locally is worse than not importing at all, because it looks
   correct in review.

Bootstrap behaviour, stated rather than silent: if `libs/ui-tokens/tokens.css`
does not exist yet, the shared tokens have not been extracted and the check is
**not applicable** — it prints a notice and passes, so the first front-end chunk is
not blocked on a refactor it did not cause. It never passes silently while the
tokens do exist.

Exit status: 0 pass (or not applicable), 1 defect, 2 usage error.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOKENS = ROOT / "libs" / "ui-tokens" / "tokens.css"

# the palette the organic direction owns; a local redefinition is drift
PALETTE = [
    "--sand", "--sand-2", "--card", "--ink", "--ink-2", "--ink-3", "--line",
    "--clay", "--clay-soft", "--sage", "--sage-soft", "--ochre", "--ochre-soft",
    "--plum", "--r-sm", "--r-ctl", "--r-lg", "--r-xl",
]

SOURCE_SUFFIXES = {".css", ".scss", ".svelte", ".vue", ".ts", ".tsx", ".js", ".jsx", ".html"}


def source_files(app: Path) -> list[Path]:
    out = []
    for p in app.rglob("*"):
        if not p.is_file():
            continue
        if any(part in {"node_modules", "dist", "build", "coverage", ".git"} for part in p.parts):
            continue
        if p.suffix in SOURCE_SUFFIXES:
            out.append(p)
    return out


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__)
        return 2

    app = ROOT / argv[1]
    if not app.is_dir():
        print(f"::error::no such directory: {argv[1]}")
        return 1

    if not TOKENS.exists():
        print(f"note: {TOKENS.relative_to(ROOT)} does not exist yet — "
              f"shared tokens not extracted, design-system check not applicable")
        return 0

    files = source_files(app)
    if not files:
        # A directory with a README and nothing else is a front end whose chunk has not
        # run. `_web.yml` makes the same distinction for the build, and reporting it as a
        # defect here would fail the gate for work that has not been scheduled yet —
        # while a directory with source that it cannot parse is still a defect below.
        print(f"note: {argv[1]} has no source files yet — nothing to check against the design system")
        return 0
    problems: list[str] = []

    # 1. at least one source file must reference the shared token file
    imported = [p for p in files if "ui-tokens/tokens.css" in p.read_text(errors="ignore")]
    if not imported:
        problems.append(
            f"{argv[1]} does not import libs/ui-tokens/tokens.css — "
            f"the design system is shared source, not a copy"
        )

    # 2. no local redefinition of a palette token
    for p in files:
        text = p.read_text(errors="ignore")
        for token in PALETTE:
            if re.search(rf"^\s*{re.escape(token)}\s*:", text, re.M):
                rel = p.relative_to(ROOT)
                problems.append(f"{rel}: redefines {token} — that belongs in libs/ui-tokens")

    if problems:
        for msg in problems:
            print(f"::error file={msg.split(':')[0]}::{msg}")
        print(f"\nFAILED: {len(problems)} design-system problem(s)")
        return 1

    print(f"OK: {argv[1]} imports the shared tokens and redefines nothing")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
