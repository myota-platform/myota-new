# Build plan

The work is tracked in **[project #3](https://github.com/orgs/myota-platform/projects/3)**,
linked to this repository. Each issue is one **chunk**: small enough to finish in
a sitting, written to be executed without further briefing.

This page exists so the sequence can be read without opening the project. The
project is authoritative — if the two disagree, the project wins and this page is
stale.

```sh
gh issue list --repo myota-platform/myota-new --label chunk --state open
gh project item-list 3 --owner myota-platform
```

## How the chunks are shaped

Every issue carries the same five sections, in the same order:

| Section | What it is for |
| --- | --- |
| **Goal** | one sentence: what exists afterwards that did not before |
| **Build** | the checklist — the actual steps |
| **Acceptance** | assertions that can be checked, not opinions |
| **Verify** | the command whose output gets pasted into the issue |
| **Out of scope** | what this chunk deliberately does not do |

Chunk keys are stable and appear in commit messages: `F` foundation, `A` admin,
`P` participant, `X` findings.

## Order

`Order` in the project is the intended sequence. Numbers are spaced by ten so a
finding can be inserted without renumbering.

### 0 · Findings — resolved

| Key | Chunk | Outcome |
| --- | --- | --- |
| X1 | Ratify ADR-0002 | **Polyglot accepted.** Recorded in ADR-0002, which exposed that `libs/ops` now exists twice; mitigated by a conformance suite. #22 closed. |
| X2 | CI runner for the chosen language | **Resolved.** Per-component workflows for Go, Rust and both front ends. #23 closed. |

With both settled, the foundation chunks are unblocked and the order below is the
order of work.

### 1 · Foundation — shared by both MVPs

Nothing here is demoable on its own; each unblocks the two MVP tracks.

| Key | Chunk | Outcome |
| --- | --- | --- |
| F1 | Repo scaffolding and toolchain | `make verify` green on an empty tree |
| F2 | Contract set and composition | `public.openapi.yaml` generated, collisions fail the build |
| F3 | Gateway | routing, `/docs`, audience check, health |
| F4 | `libs/ops` | outbox, idempotency, job claim, migrations — once |
| F5 | Three databases + PgBouncer | core, geo (PostGIS), activity; pooled |
| F6 | Deploy skeleton | `myota-test` + workspace `myota` + both hostnames live |
| F7 | CI publishes public images | digest-pinned, no floating tags |

F6 before the MVPs is deliberate: the environment exists before the first
feature, so "deploy it to test" is never a separate project at the end.

CI itself is already in place — the workflows exist and are sparse by component,
so F7 is about making the publish half work rather than writing it from scratch.

### 2 · MVP Admin

An administrator can sign in with MFA, curate the place catalogue, and define
programmes. That is the whole admin MVP: **the console has real data behind it and
a participant-facing catalogue exists to administer.**

| Key | Chunk | Outcome |
| --- | --- | --- |
| A1 | Admin auth | login + mandatory MFA + admin-audience session |
| A2 | Roles, scopes, introspection | `core` is the only place roles are evaluated |
| A3 | Admin web shell | login, MFA enrolment, five-section nav, guard |
| A4 | Admins section | the authority surface, with the subset rule |
| A5 | Participants section | support at volume, no authority |
| A6 | Programmes | programmes, entity types, immutable policy versions |
| A7 | Geodata import | presigned upload → preprocess → candidates |
| A8 | Geodata review | review queue, atomic promotion, publication events |

**Admin MVP is done when:** an operator signs in at
`https://myota-admin-test.ea7klk.es`, imports a GeoJSON fixture, reviews and
publishes the candidates, and sees the published entities in the entity list.

### 3 · MVP Participant

A participant can register, find a place, activate it, log QSOs, and see award
progress.

| Key | Chunk | Outcome |
| --- | --- | --- |
| P1 | Email flows | registration, activation, recovery, address change |
| P2 | Participant web shell | register, verify, sign in, guard |
| P3 | Public geodata API | search, entity detail, map tiles |
| P4 | Discover + map screens | browse and select a place |
| P5 | Activation + QSO | start, log, close, emit events |
| P6 | Awards | definitions, evaluation, issuance, certificates |

**Participant MVP is done when:** a new person registers at
`https://myota-test.ea7klk.es`, verifies by email, finds a published place,
activates it, logs a QSO, closes it, and sees award progress move.

## CI and CD

Per-component workflows, each with its own path filter, so a change to one service
never builds another. Shared bodies live in `_service-go.yml`, `_service-rust.yml`
and `_web.yml`.

CD is a commit: the image workflow pins a new digest in
`deploy/fleet/myota-test/values.yaml`, and Fleet reconciles it.
`smoke-test.yml` then probes both public hostnames — it needs no cluster
credential, because the check is HTTP against a public site.

## Environments

| Environment | Namespace | Participant | Admin |
| --- | --- | --- | --- |
| test | `myota-test` | `https://myota-test.ea7klk.es` | `https://myota-admin-test.ea7klk.es` |
| production | `myota` | — | — |

Both in Fleet workspace `myota`. The workspace and the test namespace were
decommissioned on 2026-11-27 and are recreated by **F6**. DNS for both hostnames
already resolves; TLS comes from Traefik's `acmeresolver`.

## Working an issue

The full agreement is `AGENTS.md` rule 14. In short: move it to In Progress,
comment the plan, comment each step, paste the `Verify` output, and create a new
issue for anything outside the plan rather than fixing it quietly.
