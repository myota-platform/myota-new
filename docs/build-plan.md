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

### 0 · Findings to settle first

| Key | Chunk | Blocks |
| --- | --- | --- |
| X1 | Ratify ADR-0002 — Go everywhere, or polyglot | A1, A7, P5 |
| X2 | CI runner for the chosen language | A1 |

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
