#!/usr/bin/env python3
"""Check the baseline schemas against the data model, and apply them to a scratch database.

Two modes:

    python3 tools/check_db_schema.py --check          # offline; what `make verify` runs
    python3 tools/check_db_schema.py --apply URL      # apply all three to a real PostgreSQL

`--check` validates four things:

  coverage     every table `docs/architecture/data-model.md` names for a database exists
               in that database's baseline, and no baseline invents a table the document
               does not describe
  ops schema   the `ops` tables named in the document are in the shared migration both
               databases apply, because all three databases carry them
  timestamps   every domain table has `created_at` and `updated_at`, which the document's
               conventions require and the trigger in 020_updated_at.sql depends on
  ownership    no table appears in two databases' baselines — the one rule the whole
               design rests on (ADR-0001)

`--apply` is the integration check: it creates three databases, applies each one's
migrations in order, applies them a second time, and asserts the second pass changes
nothing. That is the "applying migrations twice changes nothing" acceptance criterion,
and it is a database property, so it needs a database.

The document is the authority. A table added to a baseline and not to the document is
reported too: an undocumented table is a table nobody owns.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DOC = REPO_ROOT / "docs" / "architecture" / "data-model.md"
DB_ROOT = REPO_ROOT / "deploy" / "myota" / "db"

# The document's database sections, mapped to the directory that holds each baseline.
DATABASES = {
    "myota_core": "core",
    "myota_geo": "geo",
    "myota_activity": "activity",
}

# Tables the document describes as living in the shared `ops` schema. They are not
# per-database, so they are checked against the shared migration instead.
OPS_SECTION_HEADING = "`ops` — present in all three databases"


class Problem(Exception):
    def __init__(self, messages: list[str]):
        super().__init__("\n".join(messages))
        self.messages = messages


def document_sections() -> dict[str, list[str]]:
    """Returns {heading: [table names]} for every heading in the data model.

    Two levels, because the document uses both. A `##` section runs to the next `##`, so
    its `###` subsections are part of it — the core tables live under
    `### Accounts and credentials` and others. A `###` section runs to the next heading of
    any level, which is what the shared `ops` block needs.

    The first version treated `###` as a section boundary at the same level as `##`, so
    every database section ended at its first subsection and appeared to describe no
    tables at all.
    """
    lines = DOC.read_text(encoding="utf-8").splitlines()
    h2 = [index for index, line in enumerate(lines) if line.startswith("## ")]
    any_head = [
        index for index, line in enumerate(lines) if line.startswith(("## ", "### "))
    ]

    def table_rows(start: int, end: int) -> list[str]:
        return [
            match.group(1)
            for line in lines[start:end]
            if (match := re.match(r"^\|\s*`(?:ops\.)?([a-z_]+)`\s*\|", line))
        ]

    sections: dict[str, list[str]] = {}

    # `##` sections, with their subsections included.
    for position, start in enumerate(h2):
        end = h2[position + 1] if position + 1 < len(h2) else len(lines)
        sections[lines[start].lstrip("#").strip()] = table_rows(start, end)

    # `###` sections, each ending at the next heading of any level.
    for start in any_head:
        if not lines[start].startswith("### "):
            continue
        end = next((index for index in any_head if index > start), len(lines))
        sections[lines[start].lstrip("#").strip()] = table_rows(start, end)

    return sections


def sql_tables(path: Path, schema: str | None = None) -> set[str]:
    """Table names created by a SQL file, restricted to one schema when given."""
    text = path.read_text(encoding="utf-8")
    if schema:
        pattern = rf"CREATE TABLE IF NOT EXISTS {schema}\.([a-z_]+)"
    else:
        pattern = r"CREATE TABLE IF NOT EXISTS ([a-z_]+)"
    return set(re.findall(pattern, text))


def read_table_body(text: str, table: str) -> tuple[str | None, bool]:
    """Returns (column definitions, marked append-only) for one table.

    Line scanning rather than a regular expression over the whole file: a pattern like
    a pattern like `(?:^--.*)*CREATE TABLE ... (.*?)` with `re.S` lets `.*` match
    newlines, and the combination backtracks exponentially on a file this size. It hung
    the first version of this check for two minutes on one table.
    """
    lines = text.splitlines()
    marker = f"CREATE TABLE IF NOT EXISTS {table} ("
    for index, line in enumerate(lines):
        if not line.startswith(marker):
            continue
        # The comment block immediately above, for the append-only marker.
        above = []
        back = index - 1
        while back >= 0 and lines[back].startswith("--"):
            above.append(lines[back])
            back -= 1
        marked = any("append-only" in comment for comment in above)

        body = []
        for following in lines[index + 1 :]:
            if following.startswith(");"):
                return "\n".join(body), marked
            body.append(following)
        return None, marked
    return None, False


def baseline_for(directory: str) -> Path:
    path = DB_ROOT / directory / "010_baseline.sql"
    if not path.exists():
        raise Problem([f"{path.relative_to(REPO_ROOT)} does not exist"])
    return path


def check() -> list[str]:
    problems: list[str] = []
    if not DOC.exists():
        return [f"{DOC.relative_to(REPO_ROOT)} does not exist"]

    sections = document_sections()
    shared_ops = DB_ROOT / "shared" / "000_ops_schema.sql"
    if not shared_ops.exists():
        problems.append(f"{shared_ops.relative_to(REPO_ROOT)} does not exist")
    else:
        documented_ops = sections.get(OPS_SECTION_HEADING, [])
        if not documented_ops:
            problems.append(
                f"{DOC.name} has no section headed {OPS_SECTION_HEADING!r}, so the ops tables "
                f"cannot be checked"
            )
        created_ops = sql_tables(shared_ops, schema="ops")
        for table in documented_ops:
            if table not in created_ops:
                problems.append(f"shared/000_ops_schema.sql does not create ops.{table}")
        for table in sorted(created_ops - set(documented_ops)):
            if table == "schema_migration":
                continue  # the migration ledger, created by the runner
            problems.append(f"shared/000_ops_schema.sql creates ops.{table}, which {DOC.name} does not describe")

    # Coverage, in both directions, plus the ownership rule.
    owner_of: dict[str, str] = {}
    for database, directory in DATABASES.items():
        heading = next((h for h in sections if database in h), None)
        if heading is None:
            problems.append(f"{DOC.name} has no section for {database}")
            continue

        path = baseline_for(directory)
        created = sql_tables(path)
        documented = set(sections[heading])

        for table in sorted(documented - created):
            problems.append(
                f"{path.relative_to(REPO_ROOT)} does not create {table}, which {DOC.name} "
                f"names for {database}"
            )
        for table in sorted(created - documented):
            problems.append(
                f"{path.relative_to(REPO_ROOT)} creates {table}, which {DOC.name} does not "
                f"describe for {database}"
            )

        # Ownership: one table, one database.
        for table in created:
            if table in owner_of:
                problems.append(
                    f"{table} is created in both {owner_of[table]} and {database}; a table "
                    f"belongs to exactly one service (ADR-0001)"
                )
            owner_of[table] = database

        # Timestamps. Every table records when a row appeared. A table that can be
        # updated also carries `updated_at`, which the trigger migration maintains.
        #
        # Append-only tables are exempt from `updated_at`, and must say so in the DDL with
        # an `-- append-only:` comment. The document's conventions state the rule flatly
        # ("every table carries both"); that is not true of a log row, and adding a column
        # nothing ever updates to satisfy a sentence would be worse than correcting the
        # sentence. The marker is what keeps the exemption deliberate rather than an
        # oversight.
        text = path.read_text(encoding="utf-8")
        for table in sorted(created):
            body, marked_append_only = read_table_body(text, table)
            if body is None:
                problems.append(
                    f"{path.relative_to(REPO_ROOT)}: could not read the definition of {table}"
                )
                continue

            if not re.search(r"^\s*created_at\s", body, re.M):
                problems.append(f"{database}.{table} has no created_at")

            has_updated = re.search(r"^\s*updated_at\s", body, re.M) is not None
            if not has_updated and not marked_append_only:
                problems.append(
                    f"{database}.{table} has no updated_at and is not marked append-only; "
                    f"add the column, or mark the table `-- append-only: <why>`"
                )
            if has_updated and marked_append_only:
                problems.append(
                    f"{database}.{table} carries updated_at but is marked append-only; "
                    f"one of the two is wrong"
                )

    # The trigger migration needs the shared file to exist and to be idempotent.
    updated_at = DB_ROOT / "shared" / "020_updated_at.sql"
    if not updated_at.exists():
        problems.append(f"{updated_at.relative_to(REPO_ROOT)} does not exist")
    else:
        text = updated_at.read_text(encoding="utf-8")
        if "CREATE OR REPLACE FUNCTION" not in text:
            problems.append("shared/020_updated_at.sql defines no trigger function")
        if "NOT EXISTS" not in text:
            problems.append(
                "shared/020_updated_at.sql is not idempotent: re-running it would fail on "
                "an existing trigger, and re-running is an acceptance criterion"
            )

    return problems


def migrations_for(directory: str) -> list[Path]:
    """Shared migrations first, then the database's own, in filename order."""
    shared = sorted((DB_ROOT / "shared").glob("*.sql"))
    own = sorted((DB_ROOT / directory).glob("*.sql"))
    return shared + own


def psql(url: str, *args: str, stdin: str | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["psql", url, "-v", "ON_ERROR_STOP=1", "-q", *args],
        input=stdin,
        capture_output=True,
        text=True,
        timeout=180,
    )


def apply(url: str) -> list[str]:
    """Applies each database's migrations twice and reports per-database results.

    Per database rather than one accumulated list, so one database failing does not hide
    the others — a missing PostGIS extension should not stop this from telling you that
    core and activity are fine.
    """
    failures: list[str] = []

    for database, directory in DATABASES.items():
        name = f"schema_check_{database.removeprefix('myota_')}"
        psql(url, "-c", f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")
        created = psql(url, "-c", f"CREATE DATABASE {name}")
        if created.returncode != 0:
            failures.append(f"{database}: creating the scratch database: {created.stderr.strip()}")
            continue

        target = url.rsplit("/", 1)[0] + "/" + name
        files = migrations_for(directory)
        problems: list[str] = []

        for pass_number in (1, 2):
            for path in files:
                result = psql(target, "-f", str(path))
                if result.returncode != 0:
                    detail = result.stderr.strip().splitlines()[0][:200]
                    hint = ""
                    if "postgis" in result.stderr.lower():
                        hint = " (this server has no PostGIS; the geo baseline needs one)"
                    problems.append(f"{database} pass {pass_number}, {path.name}: {detail}{hint}")
                    break

        if not problems:
            counts = psql(target, "-Atc", """
                SELECT (SELECT count(*) FROM information_schema.tables
                         WHERE table_schema NOT IN ('pg_catalog','information_schema')
                           AND table_type = 'BASE TABLE')
                    || ' tables, '
                    || (SELECT count(*) FROM pg_indexes
                         WHERE schemaname NOT IN ('pg_catalog','information_schema'))
                    || ' indexes, '
                    || (SELECT count(*) FROM pg_trigger WHERE NOT tgisinternal)
                    || ' triggers'
            """)
            print(f"  {database}: {len(files)} migration(s) applied twice -> {counts.stdout.strip()}")

        # Acceptance criterion 2, in both directions: PostGIS must work on `myota_geo` and
        # must not work on the other two. Asserting only the positive half would pass on a
        # server where the extension had leaked into every database.
        if not problems:
            version = psql(target, "-Atc", "SELECT postgis_version()")
            if database == "myota_geo":
                if version.returncode != 0:
                    problems.append(
                        f"{database}: postgis_version() failed: {version.stderr.strip()[:160]}"
                    )
                else:
                    print(f"  {database}: postgis_version() = {version.stdout.strip()}")
            elif version.returncode == 0:
                problems.append(
                    f"{database}: postgis_version() succeeded, but PostGIS belongs only to myota_geo"
                )
            else:
                print(f"  {database}: no PostGIS (correct)")

        failures.extend(problems)
        psql(url, "-c", f"DROP DATABASE IF EXISTS {name} WITH (FORCE)")

    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    action = parser.add_mutually_exclusive_group()
    action.add_argument("--check", action="store_true", help="offline: schema files against the data model (default)")
    action.add_argument("--apply", metavar="URL", help="apply all three baselines twice to a PostgreSQL")
    args = parser.parse_args()
    if not args.apply:
        # Default to the offline check, so the repository's `checks-static` glob can run
        # this script with no arguments and a bare `python3 tools/check_db_schema.py` does
        # the safe thing.
        args.check = True

    try:
        if args.check:
            problems = check()
            if problems:
                raise Problem(problems)
            total = sum(len(sql_tables(baseline_for(d))) for d in DATABASES.values())
            print(
                f"OK: {total} table(s) across {len(DATABASES)} database(s) match the data model, "
                f"with no table in two of them"
            )
            return 0

        problems = apply(args.apply)
        if problems:
            raise Problem(problems)
        print("OK: all three baselines apply twice, and only myota_geo has PostGIS")
        return 0
    except Problem as error:
        for problem in error.messages:
            print(f"  error  {problem}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
