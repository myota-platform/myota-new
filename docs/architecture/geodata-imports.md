# Geodata imports

The import pipeline is the most valuable capability in the system and the most
convoluted in the legacy implementation. It is preserved in full, with the state
machine collapsed from eleven states to five and one worker process instead of
three.

## Pipeline

```
┌──────────┐   ┌───────────┐   ┌──────────┐   ┌──────────┐   ┌─────────┐
│  Upload  │──►│ Normalise │──►│ Validate │──►│ Conflate │──►│ Publish │
│  (S3)    │   │ (adapter) │   │ (bounded)│   │ (scoring)│   │ (atomic)│
└──────────┘   └───────────┘   └──────────┘   └──────────┘   └─────────┘
     │              │                │              │             │
   admin          worker           worker          worker        admin
```

### Stage 1 — Upload

An administrator creates an upload session and streams the file to S3 in parts.
The API never holds the whole file in memory.

```
POST   /v1/imports                    { sourceId, programmeId, filename, size, checksum }
POST   /v1/imports/{id}/parts/{n}     binary part + X-Part-SHA256
POST   /v1/imports/{id}/complete      → creates import_run, stage=upload, status=queued
DELETE /v1/imports/{id}               abort
```

Limits, stated once and enforced in one place:

| Limit | Value | Enforced at |
| --- | --- | --- |
| Single file | 1 GiB | upload session creation |
| Part size | 8–64 MiB | part upload |
| Parts per upload | 10 000 | session creation |
| Session lifetime | 24 h | database TTL + sweeper |

The legacy system declared a 20 MiB cap in the OpenAPI contract while the code
defaulted to 1 GiB. A single constant in `libs/limits` is imported by the
contract generator and the validator, so the contract cannot disagree with the
enforcement.

### Stage 2 — Normalise

The worker streams the object from S3 through the adapter for its format and
emits a normalised feature stream. Nothing is buffered whole.

| Format | Adapter | Parser |
| --- | --- | --- |
| GeoJSON / FeatureCollection | `geojson` | streaming |
| WFS GeoJSON | `wfs` | streaming + paging |
| ArcGIS FeatureServer | `arcgis` | streaming + paging |
| KML | `kml` | streaming |
| GPX | `gpx` | streaming |
| Shapefile (zip) | `shapefile` | streaming per `.shp` record with `.dbf` join |
| OSM PBF | `osm_pbf` | streaming, tag filter applied at read |

Each adapter produces `(source_feature_id, geometry, properties)`. Reprojection to
EPSG:4326 happens here; the database only ever stores 4326.

### Stage 3 — Validate

Per feature, bounded and cheap:

- geometry is non-empty and topologically valid (`ST_IsValid`);
- the geometry type is allowed by the programme's `geometry_policy`;
- the feature lies inside the configured area of interest, if one is set;
- required tags are present for the entity type;
- the source has a licence that permits redistribution (for remote fetches).

Failures do not abort the run. They become `import_error` rows and the candidate
is marked `invalid`, so the operator sees a complete error report after one pass
rather than discovering problems one at a time.

### Stage 4 — Conflate

For each valid candidate, find published entities within 50 m using the GiST
index, then score:

```
score = 0.45 * name_similarity
      + 0.35 * distance_score        (1.0 at 0 m, 0 at 50 m)
      + 0.20 * type_match
```

| Score | Resolution |
| --- | --- |
| ≥ 0.85 | auto-merge candidate — proposes updating the existing entity |
| 0.55–0.85 | flagged for human review |
| < 0.55 | treated as new |

This is the same scoring the legacy system used; the difference is that it
produces one `conflation_candidate` row with a reason payload instead of
threading state through a promotion queue.

### Stage 5 — Publish

The administrator reviews and promotes. Promotion is **atomic per request** and
capped at 300 candidates, which keeps the transaction bounded and the failure
mode obvious.

```
POST /v1/imports/{id}/promote   { candidateIds: [...], target: published|candidate }
```

One transaction:

1. verify every candidate is in a reviewable state;
2. for each, insert or update `entity`, write `entity_provenance`;
3. write one `geodata.entity.published.v1` per promoted entity to the outbox;
4. update counters on `import_run`;
5. set `stage=publish`, `status=completed` when nothing remains.

If any step fails, nothing is published. The legacy system promoted candidates
one at a time through a durable queue, which meant a partially published run
that had to be reconciled by hand.

## State machine

Five states. Each has a writer and a meaning.

```
queued ──► running ──► review ──► publishing ──► completed
  │           │          │           │
  └───────────┴──────────┴───────────┴──► failed (terminal, with reason)
```

| State | Meaning | Set by |
| --- | --- | --- |
| `queued` | Run created, waiting for a worker | API, on upload complete |
| `running` | Worker is normalising/validating/conflating | worker, on claim |
| `review` | Candidates ready, awaiting an administrator | worker, on success |
| `publishing` | Promotion transaction in flight | API, on promote |
| `completed` | All candidates resolved | API, after promote |
| `failed` | Unrecoverable error, reason recorded | worker or API |

The legacy machine had `UPLOAD_PENDING`, `QUEUED`, `PROCESSING`, `CANCELLING`,
`CANCELLED`, `PREPROCESSED`, `PREPROCESSED_WITH_ERRORS`, `COMPLETED`,
`COMPLETED_WITH_ERRORS`, `FAILED`, `PROCESSED`. Two of those —
`COMPLETED_WITH_ERRORS` and `UPLOAD_PENDING` — were declared but had **no writer**
anywhere in the codebase. Errors are a count on the run plus rows in
`import_error`, not a separate state.

Cancellation is a flag, not a state: `cancel_requested_at` is set by the API, the
worker checks it between batches, and the run ends in `failed` with reason
`cancelled`. This removes a state and two transitions without losing the
capability.

## Work dispatch

```
import_run (queued)
     │
     ├── outbox_event: geodata.import.queued.v1     ← same transaction
     │
     └── JetStream stream GEODATA_WORK
              durable: geodata-import
                   │
                   └── geodata-worker (N replicas)
```

One stream, one durable, one work type for imports. The legacy system had four
durables on one stream (`preprocess`, `promotion`, `entity-delete`,
`location-enrichment`) and separate code paths for each. Here all four are job
types on the `ops.job` table, dispatched through the same consumer, distinguished
by a `kind` column:

| Job kind | Trigger | Typical duration |
| --- | --- | --- |
| `import.preprocess` | upload complete | 30 s – 20 min |
| `entity.enrich` | location enrichment request | 1 – 60 s |
| `entity.delete` | deletion request | 1 – 30 s |
| `import.fetch` | refresh schedule fires | 10 s – 5 min |

One consumer loop, four handlers. Leases, heartbeats, attempt counts, and
dead-lettering exist once.

## Worker capacity

The worker is the only CPU-bound process. Its limits are explicit rather than
discovered:

| Knob | Default | Rationale |
| --- | --- | --- |
| Concurrent imports per replica | 1 | A single import already saturates a core |
| Batch size | 500 features | Bounds memory at ~40 MB per batch |
| Memory ceiling | 1.5 GiB | Kubernetes limit; the worker sheds batches and checkpoints before hitting it |
| Heartbeat | every 30 s | Lease is 300 s |
| Max deliveries | 5 | Then dead-letter with the source object preserved |

Reprocessing is safe: candidates are keyed by `(run_id, source_feature_id)`, so a
retried batch upserts rather than duplicates.

## What the participant sees

Participants do not import. They may **propose** a place:

```
POST /v1/proposals  { programmeId, name, coordinates, categoryCode, note }
```

A proposal creates a single-feature `import_run` with `source.adapter = manual`
and one candidate, which lands in the same review queue as bulk imports. The
legacy system had a separate `proposals` endpoint with its own promotion path;
here it is the same pipeline with one candidate.

## Scheduling

```
POST /v1/imports/schedules   { sourceId, cron, programmeId, autoPromote }
```

A schedule creates an `import_run` on its cron. `autoPromote` publishes
automatically only when the conflation score is high and no validation errors
exist; anything else waits for review. This replaces the separate
`geodata-importer` application.

## Deletion

Deleting an entity is an `activity`-visible event, so it is a two-phase job:

1. `activity` is asked for the impact (activation and QSO counts referencing the
   entity) through its API;
2. the entity is soft-retired, `geodata.entity.retired.v1` is published, and
   `activity` detaches references.

A hard delete runs only after the retention window and only with no remaining
references. The legacy `entity-deletion-jobs` endpoints map onto `ops.job` with
`kind = entity.delete`.
