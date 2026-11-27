# ADR-0006 — Diagrams as code: Mermaid projections over a Structurizr model

**Status:** accepted · 2026-11-27
**Supersedes:** the hand-written SVG generator in `tools/render_diagrams.py`

## Context

The first version of this repository drew its diagrams with a Python script that
emitted SVG. It worked, but it was not declarative: the script held **65
hand-placed text coordinates** across three diagrams, and producing a readable
result took four correction cycles of moving labels that collided.

That is the wrong shape. Layout is a solved problem; encoding it by hand is work
that adds no information and decays the moment anyone edits a box.

Two needs pull in different directions:

1. **A reader on github.com should see the diagram without installing anything.**
2. **The architecture should be described once, so several views can be derived
   from it** without redrawing.

A single tool does not satisfy both.

## Decision

**Two representations, with a stated authority.**

| Representation | Where | Role |
| --- | --- | --- |
| Structurizr DSL | `docs/architecture/structurizr/workspace.dsl` | **Authoritative C4 model.** People, containers, external systems, relationships, deployment, dynamic flows. |
| Mermaid blocks | inside `docs/**/*.md` | A projection of that model, for reading in a Markdown file. |

Plus a checker, `tools/check_diagrams.py`, that renders every Mermaid block and
validates the DSL structurally.

## Why Mermaid for the projection

- **GitHub renders it natively.** No build, no CI artifact, no committed image, and
  nothing to go stale between the diagram and the text beside it.
- **It is text in the same file as the prose.** A change to a paragraph and the
  diagram it describes land in one diff.
- **It renders to SVG on demand** (`mmdc`) when a slide or a PDF needs a file, so
  the readable form and the exportable form share one source.

Weakness accepted: limited layout control. Large graphs get busy. That is why the
ERD is three diagrams rather than one — a 30-table canvas is unreadable at any
size, and Mermaid makes that limitation visible early.

## Why Structurizr for the model

- **Views derive from the model.** A filtered container view for a specific
  audience, another dynamic flow, another environment's deployment view — each is
  a few lines, not a redrawn picture.
- **It encodes the things a flowchart cannot say.** Container technology, tags,
  deployment nodes, and ordered dynamic flows are first-class.
- **It makes architectural claims checkable.** A dynamic-view step must be a
  declared relationship, or the checker fails. A drawn arrow can lie quietly; a
  declared relationship cannot.
- **It exports to Mermaid.** `structurizr-cli export -format mermaid` is the bridge
  that eventually removes the duplication entirely.

## The cost, stated plainly

**Two representations drift.** Until the CLI is available in CI, the Mermaid blocks
are maintained beside the model rather than generated from it.

Two mechanisms address this:

1. A rule in [`AGENTS.md`](../../AGENTS.md) requiring both to be updated in the
   same commit, with a table mapping each kind of change to the diagram it must
   touch.
2. `tools/check_diagrams.py`, which proves each representation is individually
   well-formed. It cannot prove they agree.

The permanent fix is `structurizr-cli export -format mermaid`, which needs a Java
runtime in CI. That is the next step.

## Alternatives considered

| Option | Verdict |
| --- | --- |
| Hand-written SVG generator (previous) | Rejected. 65 hand-placed coordinates, four iterations to fix collisions, and layout decisions encoded as data. |
| Graphviz DOT | Viable. Deterministic and mature, but the output is ugly by default and it has no notion of a model — no technology, no deployment, no dynamic flow. |
| D2 | Best layout quality of the text formats and a pleasant syntax, but GitHub does not render it, so it needs a CI step producing an artifact. |
| PlantUML | Rejected: needs Java or a rendering server, and GitHub will not render it. |
| Structurizr only, exported to images | Rejected as the *only* representation: it would commit generated images and make the docs unreadable without a build. Kept as the model layer instead. |
| Mermaid only | Rejected: no model, so every view is drawn separately and there is no place to state "this container uses PostGIS" except prose. |
| DBML for the schema | Considered, and still attractive — it can emit both an ERD and baseline DDL. Not adopted yet because the schema is not implemented; revisit when `deploy/db/migrations` exists. |
| Kroki (self-hosted multi-format renderer) | Rejected: another service on a single-node cluster for a problem that `mmdc` solves locally. |

## Consequences

**Gained**

- A change to prose and its diagram is one diff.
- Nine diagrams across seven documents, all rendered by GitHub with no build.
- The architecture is described once, in a model that can produce further views.
- A checker with teeth: it caught a hand-injected undeclared dynamic step and a
  malformed node during its own negative test.

**Cost**

- Dual maintenance until CI has Java.
- Contributors must know two syntaxes, though only one of them (Mermaid) is needed
  to fix a diagram in a document.
- The DSL is structurally validated but **not compiled** on this host — no Java
  runtime. That gap is stated in the Structurizr README rather than hidden.

## Verification

`python3 tools/check_diagrams.py --full`:

- renders every Mermaid block and fails on any parse error;
- **compiles the DSL with `structurizr-cli`** when one is available;
- otherwise falls back to heuristics: brace balance, argument arity, declared
  identifiers, `containerInstance` targets, and dynamic-view steps;
- prints explicitly what it could not verify.

Negative tests performed:

| Injected fault | Heuristics | CLI |
| --- | --- | --- |
| Undeclared relationship in a dynamic view | caught | caught |
| Malformed Mermaid node | caught | n/a |
| `softwareSystem` with a technology argument (arity) | caught | caught |
| `!identifiers hierarchical` breaking name resolution | **missed** | caught |

That last row is the reason the CLI layer exists. It is not decoration.

## Postscript: the model shipped broken

The first version of `workspace.dsl` was committed with a claim that it was
"structurally validated". It was not valid: two `softwareSystem` declarations
carried a technology argument, which the parser rejects as *too many tokens*, and
`!identifiers hierarchical` made every container reference from outside the
software system unresolvable.

The heuristic checker printed `OK: Structurizr model structurally valid` for an
invalid file. That is the worst failure mode a check can have — a false pass is
worse than no check, because it transfers confidence that was never earned.

Three changes follow from it:

1. **The CLI is now the authority.** Heuristics run first, because they give better
   error messages, but `--full` compiles the file and fails if it cannot.
2. **The summary line states which layer actually ran** — "compiled" or "checked
   heuristically" — instead of a blanket "valid".
3. **`STRUCTURIZR_CLI=none` exists** so the heuristic-only path is testable rather
   than assumed, which is how the missed-fault row above was established.

The general lesson, which applies past this file: a check must be able to report
that it verified nothing. An unqualified success message that can be printed for
broken input is worse than a failure.
