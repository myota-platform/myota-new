#!/usr/bin/env python3
"""Pin one service's image digest in a Fleet values file.

    python3 tools/pin_image.py \\
        --file deploy/fleet/myota-test/values.yaml \\
        --service gateway \\
        --image ghcr.io/myota-platform/myota-new-gateway \\
        --digest sha256:0123...cdef

Called by `.github/workflows/_image.yml` after a push. It is a tool rather than an inline
`sed` because it has to be two things at once:

  * **comment preserving** — the values file is hand-written prose plus configuration, and a
    YAML round-trip through a parser would strip every explanation from it;
  * **idempotent and ordered** — two services publishing on the same commit run this twice,
    and the same commit re-run runs it again. The result must be byte-identical each time,
    and the keys must stay in a stable order so the diff shows one changed line.

The block it maintains:

    images:
      activity: ghcr.io/myota-platform/myota-new-activity@sha256:...
      gateway: ghcr.io/myota-platform/myota-new-gateway@sha256:...

A digest, never a tag: a tag is a pointer that can move, and a deployment that can change
without a commit is a deployment nobody reviewed.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

DIGEST_PATTERN = re.compile(r"^sha256:[0-9a-f]{64}$")
IMAGE_PATTERN = re.compile(r"^[a-z0-9.-]+(?::[0-9]+)?/[a-z0-9._/-]+$")
SERVICE_PATTERN = re.compile(r"^[a-z][a-z0-9-]*$")

BLOCK_HEADER = "images:"
KEY_INDENT = "  "


def fail(message: str) -> None:
    sys.exit(f"pin_image: {message}")


def find_block(lines: list[str]) -> tuple[int, int]:
    """Return (header index, end index) for the top-level `images:` block.

    `end` is the index of the first line after the block: the next top-level key, or the
    end of the file. Blank lines and comments inside the block belong to it.
    """
    header = next((index for index, line in enumerate(lines) if line.rstrip() == BLOCK_HEADER), None)

    if header is None:
        # No block: append one. This is the path a new environment takes before its first
        # service is published, and it keeps the tool usable without a hand-written header.
        while lines and lines[-1].strip() == "":
            lines.pop()
        lines.extend(["", "# Image digests written by CI. Do not edit by hand.", BLOCK_HEADER])
        return len(lines) - 1, len(lines)

    end = len(lines)
    for index in range(header + 1, len(lines)):
        line = lines[index]
        if line.strip() and not line.lstrip().startswith("#") and indent_of(line) == 0:
            end = index
            break

    return header, end


def entry_line(service: str, reference: str) -> str:
    return f"{KEY_INDENT}{service}: {reference}"


def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def pin(lines: list[str], service: str, reference: str) -> bool:
    """Set `service` to `reference` inside the images block. Returns True when it changed."""
    header, end = find_block(lines)

    # The block's own entries are the lines indented deeper than the header.
    entries: list[tuple[int, str]] = []
    for index in range(header + 1, end):
        line = lines[index]
        if line.strip() == "" or line.lstrip().startswith("#"):
            continue
        name, _, _ = line.strip().partition(":")
        entries.append((index, name.strip()))

    for index, name in entries:
        if name == service:
            if lines[index] == entry_line(service, reference):
                return False
            lines[index] = entry_line(service, reference)
            return True

    # Insert, keeping the entries in name order so the file diffs predictably.
    if not entries:
        # An empty block: the new entry goes directly under the header. This also keeps it
        # above any comment that follows the block, which is where the next key would land
        # if "insert at the end" were taken literally.
        lines.insert(header + 1, entry_line(service, reference))
        return True

    insertion = None
    for index, name in entries:
        if name > service:
            insertion = index
            break
    if insertion is None:
        # After the last entry rather than at the block's end: trailing commentary belongs
        # to the file, not to the mapping.
        insertion = entries[-1][0] + 1

    lines.insert(insertion, entry_line(service, reference))
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--file", required=True, help="values file to edit")
    parser.add_argument("--service", required=True, help="key under images:, e.g. gateway")
    parser.add_argument("--image", required=True, help="registry path without tag or digest")
    parser.add_argument("--digest", required=True, help="sha256:<64 hex characters>")
    arguments = parser.parse_args()

    if not SERVICE_PATTERN.match(arguments.service):
        fail(f"--service {arguments.service!r} is not a lowercase service name")
    if not IMAGE_PATTERN.match(arguments.image):
        fail(f"--image {arguments.image!r} is not a registry path")
    if not DIGEST_PATTERN.match(arguments.digest):
        fail(f"--digest {arguments.digest!r} is not sha256:<64 hex>; a tag is not a pin")

    path = Path(arguments.file)
    if not path.is_file():
        fail(f"{path} does not exist")

    original = path.read_text(encoding="utf-8")
    lines = original.splitlines()
    reference = f"{arguments.image}@{arguments.digest}"

    if not pin(lines, arguments.service, reference):
        print(f"pin_image: {arguments.service} already at {arguments.digest[:19]}…; unchanged")
        return 0

    updated = "\n".join(lines) + "\n"
    path.write_text(updated, encoding="utf-8")
    print(f"pin_image: {path}: images.{arguments.service} = {reference}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
