# What was removed and why

The brief was to keep the old functionality and remove as much clutter and
complexity as possible, avoiding duplication. This is the ledger: every
simplification, what it replaces, and what it costs.

## Processes: 12 → 6

| Legacy process | Now |
| --- | --- |
| `gateway` | `gateway` (rewritten, now owns contracts) |
| `identity` | `core-api` |
| `programmes` | `core-api` (same service — see below) |
| `activity` | `activity-api` |
| `activity-notifications` | `activity-worker` (one job kind) |
| `activity-worker` | `activity-worker` |
| `geodata` | `geodata-api` |
| `geodata-importer` | `geodata-worker` (job kind `import.fetch`) |
| `geodata-worker` | `geodata-worker` |
| `email-worker` | `core-worker` (job kind `notification.send`) |
| `identity-maintenance` | `core-worker` (job kind `account.retention`) |
| `outbox-worker` | relay mode in each service |

Each of the removed processes existed to run one loop. `activity-notifications`
consumed one durable and wrote to one table. `email-worker` consumed one subject
and sent SMTP. `identity-maintenance` ran a nightly sweep. `outbox-worker` was a
relay for all three databases at once, which was itself a boundary violation — a
single process holding credentials for every database.

Six processes, each with a reason to be separate.

## Duplication removed

### The operational quartet was copied three times

`outbox_event`, `idempotency_record`, `consumer_checkpoint`, and `dead_letter`
existed in all three databases, with a separate relay, a separate consumer
harness, and a separate recovery sweep per service. Twelve applications each
carried their own copy of the retry loop.

Now: one `libs/ops` implementing the pattern once, parameterised by database URL
and owner name. The tables remain per-database (they must — they are written in
the same transaction as the domain change), but the code is written once.

### Notifications existed in two databases

`myota_core` had `email_delivery`, `email_delivery_attempt`, `email_dead_letter`,
`email_dead_letter_redrive`. `myota_activity` had `activity_notification` and
`activity_notification_redrive`. Two ledgers, two retry mechanisms, two
dead-letter redrive CLIs, for one responsibility.

Now: `core.notification` plus `core.notification_delivery_attempt`, and the
shared `ops.dead_letter`. Every service that needs to notify publishes
`core.notification.requested.v1`. `core` is the only sender.

### Programmes existed twice

`core.programme` (uuid-keyed, baseline era) and `core.programme_record`
(text-keyed, JSONB era) both described a programme. `core.entity_type` and
`core.programme_entity_type` both described a category. Two naming conventions,
two ID types, one concept.

Now: `core.programme` with a uuid id and a unique slug, and
`core.programme_entity_type` only. Geo's `entity_type` table is deleted; category
codes come from `core`.

### Three staging mechanisms in geodata

`geodata_edit_staging`, `geodata_control_record`, and `geodata_schema_feature`
each held partially-parsed features in different shapes. Imports wrote to one,
edits to another, and the review UI had to understand all three.

Now: `import_candidate` for import work, and direct writes for edits. Edits do not
need staging because they are single entities with optimistic concurrency
(`revision`).

### Two snapshot tables

`source_snapshot_manifest` (one row per fetch) and `source_snapshot_record` (one
row per feature) are collapsed into `source_snapshot` with a `feature_count`, and
provenance is expressed by `entity_provenance` pointing at the snapshot plus a
source feature id.

### Queue state in a domain table

`geodata_import_processing_queue` held queue rows with `status`, `attempt_count`,
`lease_until`, `heartbeat_at` — a work queue implemented as a domain table, next
to the real work queue in JetStream, and a second real one in `activity_job`.

Now: `ops.job` with a `kind` discriminator. One lease mechanism, one retry policy,
one dead-letter path, one redrive tool.

## States removed

| Machine | Before | After |
| --- | --- | --- |
| Import run | 11 states | 5 states + `failed` |
| Award request | draft, submitted, under_review, approved, rejected, issued, expired, cancelled | `requested`, `approved`, `rejected`, `issued` |
| Activation | active, closing, closed, cancelled, reconciled | `active`, `closed`, `cancelled` |

Two import states — `UPLOAD_PENDING` and `COMPLETED_WITH_ERRORS` — were declared
in the type and had **no writer in the codebase**. They were reachable only by a
direct database update. Removing them removes a way for the system to be in a
state nobody designed.

## Tables removed

| Table | Reason |
| --- | --- |
| `core.activation`, `core.qso`, `core.award`, `core.award_rule` | Legacy monolith leftovers that survived the baseline migration and were never dropped. Activity owns these. |
| `core.service_state` | Python-era JSONB serialisation dump, superseded by real tables. |
| `core.identity_casefold_mapping` (1 540 rows) | A Unicode 15.0 case-folding table shipped as data. Replaced by `citext` plus a normalised `email_key`. |
| `core.identity_auth_state_migration` | A one-off backfill marker. Irrelevant in a fresh schema. |
| `core.oidc_provider_mapping` | Superseded by `identity_oidc_provider_mapping`; the old table was never dropped. |
| `geo.entity_type` | Duplicate of `core.programme_entity_type`. |
| `geo.geodata_edit_staging`, `geodata_control_record`, `geodata_schema_feature` | Three staging tables, one concept. |
| `geo.source_snapshot_manifest`, `source_snapshot_record` | Collapsed into `source_snapshot`. |
| `geo.geodata_import_processing_queue` | Queue state belongs in `ops.job`. |
| `geo.geodata_import_candidate_stage` | Second staging table for candidates. |
| `activity.activity_subject_aggregate`, `activity_subject_callsign`, `activity_subject_entity` | Three denormalised projection tables; replaced by one `statistic_snapshot` with a `scope` column. |
| `activity.activity_import` + `activity_job` | Both were queue/import bookkeeping; folded into `log_import` and `ops.job`. |

Net: roughly 30% fewer tables, with no capability lost.

## Contracts: 4 documents → 1 published

Four `openapi.yaml` files existed; none was served. The gateway now composes them
into one published document with a playground. Service documents remain as the
source of truth but are internal.

## Sealed Secrets removed

The cluster runs Sealed Secrets today, and the infra repository already documents
its removal. `myota-new` does not use it at all — see
[secrets-and-config.md](secrets-and-config.md).

## What was deliberately kept

Some complexity is essential and removing it would cost capability:

| Kept | Why |
| --- | --- |
| Three databases | Isolation, independent backup, independent failure. Merging them would couple the largest and smallest workloads. |
| Transactional outbox | The only correct way to publish an event atomically with a state change. |
| Separate admin and participant front ends | Different audiences, different lifetimes, different performance budgets. |
| Full import pipeline | It is the core value of the geodata service. |
| PostGIS | Reinventing spatial indexing is not a simplification. |
| Resumable multipart upload | A 1 GiB shapefile over a poor connection is a real case. |
| Idempotency keys on mutations | Retries happen; the alternative is duplicate awards. |
| MFA for admins | The only change here that *adds* friction, and it is worth it. |

## What it costs

This design is simpler in structure and more demanding in discipline:

- **Three languages** if the polyglot option is taken (see
  [technology.md](technology.md)). The default Go-everywhere option avoids this.
- **Eventual consistency.** `activity` may briefly show a place name from before an
  edit. Mitigated by embedding the name at activation start.
- **No cross-database joins.** A screen that needs entities and activations makes
  two calls. The gateway does not join them.
- **A fresh schema.** The legacy data is not migrated; the decommission discarded
  it. This is a rebuild, not a migration.
