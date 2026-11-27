# ADR-0001 — Service boundaries follow the databases

**Status:** accepted · 2026-11-27

## Context

The previous architecture had four domain services and three databases:
`identity` and `programmes` both wrote `myota_core`. Neither could claim sole
ownership of the schema, so ownership had to be restated in prose and
re-checked by hand whenever someone added a table. Every role check that
involved a programme became either a cross-service call or a violation.

At the same time, the operational plumbing — outbox, idempotency checkpoints,
consumer offsets, dead-letter handling — was implemented once per service,
across twelve deployable applications, with three separate copies of the schema
and three separate recovery sweeps.

## Decision

A service is defined by the database it exclusively owns.

| Service | Database | Deployment modes |
| --- | --- | --- |
| `core` | `myota_core` | api, worker, relay |
| `geodata` | `myota_geo` (PostGIS) | api, worker, relay |
| `activity` | `myota_activity` | api, worker, relay |

Consequences that follow directly:

1. **Identity and Programmes merge into `core`.** Programme configuration is
   authorization input: `account_role.programme_id` and `approver_scope` scope a
   grant to a programme. One service means the check is a join, not a round trip
   that cannot participate in the transaction.
2. **No other service may connect to a database it does not own.** Enforcement is
   mechanical: each deployment mounts exactly one database secret.
3. **Cross-service identifiers are opaque UUIDs without foreign keys.** There are
   no distributed transactions. Ordering is a property of the event stream.
4. **Operational tables stay per-database but the code is shared.** They must be
   written in the same transaction as the domain change, so they cannot be
   centralised; the implementation lives once in `libs/ops`.
5. **One relay per database**, not one relay for all three. The old
   `outbox-worker` held credentials for every database — a boundary violation
   disguised as a convenience.

## Alternatives considered

**Four services on three databases (status quo).** Rejected: shared-schema
ownership is not enforceable, and the role-check problem is structural.

**Three databases, with identity and programmes split by schema within
`myota_core`.** Rejected: a schema boundary without a separate connection pool
and separate migrations is naming, not isolation. It would still permit a
cross-schema join, which is exactly the coupling being removed.

**One database, schema-per-service.** Rejected: independent backup, independent
failure, and independent sizing are the reasons the three databases exist. A
single instance couples the smallest and largest workloads' maintenance windows.

**Seven services, one per bounded context (awards separate from activity, imports
separate from geodata).** Rejected: each additional service multiplies the
deployment, secret, migration, and monitoring surface without adding a data
boundary. Awards are computed from QSO data that lives in `activity`; splitting
them creates a synchronous dependency in the evaluation path for no isolation
benefit.

## Consequences

**Gained**

- Ownership is a fact of the deployment, not a rule in a document.
- One implementation of outbox/idempotency/dead-letter, used by three services.
- Twelve deployable processes become six.
- A role check is transactional.

**Cost**

- `core` carries two domains and will be the largest service. If Programmes grows
  to justify its own database, it becomes its own service — the boundary moves
  with the data, which is the point.
- Cross-domain screens make two calls. The gateway does not join.
- Eventual consistency between `activity` and `geodata`; mitigated by capturing
  the entity name at activation start.

## Verification

The design is only real if the deployment enforces it. Checks:

1. Each service's Kubernetes manifest references exactly one database secret.
2. No service's connection string appears in another service's configuration.
3. Migrations run per database, from a directory owned by that service.
4. A CI check greps the built images for foreign database hostnames.
