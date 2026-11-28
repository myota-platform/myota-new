# Decision records

Architecture Decision Records for MyOTA. One file per decision, numbered
sequentially, never renumbered and never deleted — a superseded ADR stays and
points at its replacement.

## What an ADR is for

A decision that is expensive to reverse, or that a future contributor would
otherwise have to re-litigate from first principles. Concretely:

- a service, database, or module boundary
- a technology or framework choice
- a protocol or delivery guarantee
- a security or authentication model
- a deployment or secrets mechanism
- a stated visual or interaction language
- anything that contradicts an existing rule in [`AGENTS.md`](../../AGENTS.md)

A decision that only affects one function does not need an ADR. A decision that
someone will ask "why is it like this?" about in six months does.

## Status vocabulary

| Status | Means |
| --- | --- |
| **proposed** | Written down and open for objection. Not binding. Nothing should be built against it. |
| **accepted** | In force. `AGENTS.md` should encode it as a rule. |
| **superseded** | Replaced by a later ADR; the file states which. Kept for history. |
| **rejected** | Considered and declined; the reasoning is the value. |

Every ADR carries a **Provenance** line stating who decided and on what basis —
"directed" with the request quoted, or "proposed" with no approval recorded. That
line exists because "Status: accepted" is a claim, and a claim needs a source.
An ADR marked accepted with provenance "proposed" is a working assumption, and
saying so is more useful than rounding it up.

## Structure

```
# ADR-NNNN — Title

**Status:**     proposed | accepted | superseded | rejected · YYYY-MM-DD
**Provenance:** who decided, and on what basis
**Supersedes:** ADR-MMMM (only when applicable)

## Context        what situation forced a choice
## Decision       what was decided, in one paragraph a reader can act on
## Alternatives   what else was considered, and why it lost
## Consequences   what it gains, and what it costs — costs are mandatory
## Verification   how to check the decision is still true
```

The alternatives and consequences sections are the point. A decision record with
no alternatives is a press release; one with no costs is advocacy.

## The records

| ADR | Decision | Status |
| --- | --- | --- |
| [0001](0001-service-boundaries-follow-databases.md) | Service boundaries follow the databases; Identity and Programmes merge into `core` | accepted |
| [0002](0002-language-per-service.md) | Polyglot per service; Go everywhere as the escape hatch | accepted |
| [0003](0003-no-sealed-secrets.md) | No Sealed Secrets; SOPS with age instead | accepted |
| [0004](0004-drop-message-broker.md) | Remove the message broker; the outbox is the event log | accepted |
| [0005](0005-horizontal-scaling.md) | Every workload is horizontally scalable | accepted |
| [0006](0006-diagrams-as-code.md) | Mermaid projections over a Structurizr model | accepted |
| [0007](0007-ui-direction-organic.md) | The UI direction is 04‑Organic | accepted |
| [0008](0008-gateway-token-verification-keys.md) | The gateway verifies tokens with keys published by `core` | accepted |
| [0009](0009-three-buckets-per-environment.md) | Three buckets per environment, one credential per environment | accepted |
| [0010](0010-workspace-and-production-share-a-name.md) | The Fleet workspace and the production namespace are both `myota` | accepted |
| [0011](0011-chart-published-to-ghcr.md) | The chart is a published OCI artifact, pinned by version | accepted |

## Open items

None. Every record is accepted, and each is reflected in `AGENTS.md`.

## How they connect to `AGENTS.md`

An accepted ADR should be encoded as a rule in [`AGENTS.md`](../../AGENTS.md), and
the rule should link back. The mapping:

| ADR | AGENTS.md rule |
| --- | --- |
| 0001 | §2 Microservices |
| 0002 | §6 Technology |
| 0003 | §8 Deployment → configuration and secrets |
| 0004 | §3 Communication between services via pubsub |
| 0005 | §12 Horizontal scaling is a correctness property |
| 0006 | §13 Diagrams are part of the change |
| 0007 | §7 The UI style is 04‑Organic |
| 0008 | §1 API First · §9 Separate admin and participant surfaces |
| 0009 | §12 Horizontal scaling · object storage |
| 0010 | §8 Deployment · the workspace and production share a name |
| 0011 | §8 Deployment · the chart is published, not vendored |

`tools/check_adrs.py` verifies every ADR has the required sections, a valid
status, and a matching reference in `AGENTS.md`.
