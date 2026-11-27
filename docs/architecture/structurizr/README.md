# Structurizr model

`workspace.dsl` is the **authoritative** C4 model of MyOTA: people, the software
system, its containers, the external systems it depends on, and every
relationship between them, including a deployment view for the local K3s cluster.

The Mermaid diagrams in the sibling documents are a **projection** of this model,
written for readability inside a Markdown file. When the two disagree, this file is
right and the diagram is stale.

## Status: compiled and exported

The DSL is validated with the Structurizr CLI (`2025.11.09`) and exports six views
to Mermaid. That is not a claim about intent — it is the result of running the
parser:

```
$ python3 tools/check_diagrams.py --full
Structurizr: docs/architecture/structurizr/workspace.dsl
  ok    braces balanced
  ok    14 element(s) declared
  ok    19 relationship(s) declared
  ok    dynamic view 'ImportPipeline': 9 step(s) all declared
  ok    dynamic view 'EventDelivery': 3 step(s) all declared
  ok    structurizr-cli compiled the workspace
```

To reproduce, the CLI needs a Java runtime and one download:

```sh
apt-get install -y openjdk-17-jre-headless
gh release download --repo structurizr/cli --pattern structurizr-cli.zip --dir /tmp/scli
cd /tmp/scli && unzip -oq structurizr-cli.zip && ./structurizr.sh version
export STRUCTURIZR_CLI=/tmp/scli/structurizr.sh
```

Or paste the file into <https://structurizr.com/dsl> — no install at all.

## Grammar traps that cost a debugging round

These produced errors in the Structurizr playground **after** the model had already
passed a brace-and-identifier check. They are recorded because they are easy to
repeat.

**1. Tags are one comma-separated string, not several arguments.**

```
// rejected: too many tokens, expected: softwareSystem <name> [description] [tags]
objectStorage = softwareSystem "Object Storage" "desc" "S3-compatible" "External"

// accepted
objectStorage = softwareSystem "Object Storage" "desc" "External"
```

**2. `softwareSystem` has no technology field.** Its signature is
`<name> [description] [tags]`. A technology string is read as a fourth token and
rejected. Only `container`, `component`, `deploymentNode` and
`infrastructureNode` take a technology argument.

| Type | Signature |
| --- | --- |
| `person` | name, description, tags |
| `softwareSystem` | name, description, tags — **no technology** |
| `container` | name, description, technology, tags |
| `component` | name, description, technology, tags |
| `infrastructureNode` | name, description, technology, tags |
| `deploymentNode` | name, description, technology, instances, tags |

**3. The DSL is line-oriented.** `deployment s "Live" { include * }` on one line is
a token error; `{` must end the line.

**4. `!identifiers hierarchical` changes how names resolve.** With it enabled, a
container declared inside a software system is reachable from outside only as
`myota.<name>`. The model omits it deliberately: every element name here is unique,
and flat identifiers keep the relationship list readable.

**5. View keys must match `[a-zA-Z0-9_-]`.** A deployment view's environment name
doubles as its key, so an environment called `"Local K3s"` needs an explicit key:
`deployment myota "Local K3s" "LocalK3s" "description" { ... }`.

**6. Themes are fetched over the network.** `theme default` requires a reachable
`static.structurizr.com`, which fails on an offline build host. It is omitted; the
default theme is applied anyway.

## What the checker can and cannot see

`tools/check_diagrams.py` runs two layers over the DSL:

| Layer | Catches | Blind to |
| --- | --- | --- |
| Heuristics (always) | unbalanced braces, arity (`too many tokens`), undeclared identifiers in relationships, `containerInstance` of a non-existent container, dynamic-view steps that are not declared relationships | identifier *resolution*, view-key syntax, environment matching, anything the grammar enforces beyond token count |
| `structurizr-cli` (`--full`) | everything the parser enforces | nothing — it is the parser |

The distinction is not theoretical. The hierarchical-identifier bug in trap 4
passed the heuristic layer and was caught only by the CLI. **Use `--full` in CI.**

## Views defined

| View | Key | What it answers |
| --- | --- | --- |
| System context | `SystemContext` | Who uses MyOTA, and what does it depend on |
| Containers | `Containers` | What runs, and in what technology |
| Data ownership | `DataOwnership` | Which container owns which database |
| Import pipeline | `ImportPipeline` (dynamic) | The ordered steps of an import, end to end |
| Event delivery | `EventDelivery` (dynamic) | How a fact reaches another service with no broker |
| Deployment | `LocalK3s` | What runs in namespace `myota` on the cluster |

Export them with:

```sh
structurizr-cli export -workspace workspace.dsl -format mermaid -output out
structurizr-cli export -workspace workspace.dsl -format plantuml -output out
```

Formats worth knowing: `mermaid`, `plantuml`, `dot`, `dsl`, `json`.

## The two-representation problem

The model exists twice: once as DSL, once as Mermaid blocks in the documents. That
is duplication, and duplication drifts. Two things keep it honest:

1. **Rule 10 in [`AGENTS.md`](../../../AGENTS.md)** — a change to a decision, a
   component, or an implementation must update both representations in the same
   commit.
2. **`tools/check_diagrams.py --full`** — compiles the DSL and renders every
   Mermaid block. It cannot prove the two agree; it proves each is individually
   valid.

The permanent fix is to generate the Mermaid from the DSL, which now works. The
remaining step is wiring the export into CI and committing the generated blocks
instead of hand-writing them. Until then, the rule and the checker hold the line.

## Why the model is authoritative

A diagram is a view. The model is the thing being viewed. Encoding the system once
means a new view — a filtered container view for a specific audience, a dynamic
view for a different flow, a deployment view for another environment — costs a few
lines rather than a redrawn picture.

It also makes an architectural claim testable: if `activity -> adminWeb` appears in
a dynamic view but not in the model, the checker fails. A drawn arrow can lie; a
declared relationship that the parser validates cannot lie quietly.
