# Structurizr model

`workspace.dsl` is the **authoritative** C4 model of MyOTA: people, the software
system, its containers, the external systems it depends on, and every
relationship between them, including a deployment view for the local K3s cluster.

The Mermaid diagrams in the sibling documents are a **projection** of this model,
written for readability inside a Markdown file. When the two disagree, this file
is right and the diagram is stale.

## Status: prepared, not rendered

Structurizr is not installed on the build host — there is no Java runtime — so the
DSL has been structurally validated but **not compiled**. Do not assume it renders
until someone has run it once.

What has been verified:

- braces balance;
- every identifier used in a relationship is declared;
- every `containerInstance` names a declared container;
- every step in each `dynamic` view corresponds to a relationship declared in the
  model — this is the check that catches a flow diagram inventing a connection
  the architecture does not have.

What has **not** been verified: that Structurizr's own parser accepts the file.

## Rendering it

Any one of these produces the views:

```sh
# with Java 17+ and the CLI
structurizr-cli export -workspace workspace.dsl -format mermaid -output out
structurizr-cli export -workspace workspace.dsl -format plantuml -output out

# with Docker, no local Java
docker run --rm -v "$PWD":/usr/local/structurizr structurizr/cli \
  export -workspace workspace.dsl -format mermaid -output /usr/local/structurizr/out
```

Or paste the file into <https://structurizr.com/dsl> — it renders in the browser
with no install.

Formats worth knowing: `mermaid`, `plantuml`, `dot`, `dsl`, `json`. The
**`mermaid` export is the bridge** between this model and the diagrams in the
documents: once the CLI is available, the Mermaid blocks can be generated from
the model instead of maintained beside it.

## Views defined

| View | Key | What it answers |
| --- | --- | --- |
| System context | `SystemContext` | Who uses MyOTA, and what does it depend on |
| Containers | `Containers` | What runs, and in what technology |
| Data ownership | `DataOwnership` | Which container owns which database |
| Import pipeline | `ImportPipeline` (dynamic) | The ordered steps of an import, end to end |
| Event delivery | `EventDelivery` (dynamic) | How a fact reaches another service with no broker |
| Deployment | `LocalK3s` | What runs in namespace `myota` on the cluster |

## The two-representation problem

Until the CLI is available, the model exists twice: once as DSL, once as Mermaid.
That is duplication, and duplication drifts. Two things keep it honest:

1. **The rule in [`AGENTS.md`](../../../AGENTS.md)** — a change to a decision, a
   component, or an implementation must update both representations in the same
   commit. A commit that updates one and not the other is incomplete.
2. **`python3 tools/check_diagrams.py`** — renders every Mermaid block and
   structurally validates the DSL. It cannot prove the two agree; it proves each
   is individually well-formed.

The permanent fix is to generate the Mermaid from the DSL. That requires a Java
runtime in CI, and it is the next step, not a current capability.

## Why the model is authoritative

A diagram is a view. The model is the thing being viewed. Encoding the system once
means a new view — a filtered container view for a specific audience, a dynamic
view for a different flow, a deployment view for another environment — costs a few
lines rather than a redrawn picture.

It also makes an architectural claim testable: if `activity -> adminWeb` appears
in a diagram but not in the model, the checker fails. A drawn arrow can lie; a
declared relationship that the checker validates cannot lie quietly.
