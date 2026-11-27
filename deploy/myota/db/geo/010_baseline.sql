-- Baseline schema for `myota_geo`. Owned by `geodata`; the only database with PostGIS.
--
-- Derived from docs/architecture/data-model.md, section "`myota_geo` — owned by
-- `geodata`". `tools/check_db_schema.py` asserts every table the document names here
-- appears in this file.
--
-- `programme_id` and `actor_id` are opaque uuids with no foreign key: `core` owns those
-- rows, and referential integrity across the boundary is a property of the event stream,
-- not of Postgres. A foreign key here would make one database's migration depend on
-- another's availability, which is the coupling ADR-0001 exists to prevent.

-- Both extensions go first, before anything that uses them. `pg_trgm` is not part of
-- PostGIS, and an index declared with `gin_trgm_ops` before the extension exists fails at
-- creation time — which is how this file failed its first run.
CREATE EXTENSION IF NOT EXISTS postgis;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- ───────────────────────────────────────────────────────────────────────────────
-- Entities — the catalogue
-- ───────────────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS entity (
    id                  uuid        PRIMARY KEY,
    -- → ext:core. Opaque: no foreign key crosses the database boundary.
    programme_id        uuid        NOT NULL,
    entity_type_code    text        NOT NULL,
    name                text        NOT NULL,
    slug                text        NOT NULL,
    status              text        NOT NULL DEFAULT 'draft',
    -- Geometry in WGS 84 and nothing else. Reprojection happens in the import adapter,
    -- so the database never holds two coordinate systems that a query could mix.
    geom                geometry(Geometry, 4326),
    -- Derived, not authored: a map viewport query filters on the centroid, and computing
    -- it per request over a polygon is work the database can do once.
    centroid            geometry(Point, 4326),
    bbox                geometry(Polygon, 4326),
    elevation_metres    numeric(7, 2),
    -- Optimistic concurrency. Two reviewers editing one park is a normal day, and the
    -- second must be told to re-read rather than silently overwrite.
    revision            integer     NOT NULL DEFAULT 1,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    published_at        timestamptz,
    retired_at          timestamptz,
    CONSTRAINT entity_status_known CHECK (status IN ('draft', 'in_review', 'published', 'retired')),
    CONSTRAINT entity_revision_positive CHECK (revision > 0),
    CONSTRAINT entity_slug_shape CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$')
);

CREATE UNIQUE INDEX IF NOT EXISTS entity_programme_slug ON entity (programme_id, slug);
-- GiST on both, because a viewport query filters on the centroid and a precise
-- containment query filters on the geometry. One index cannot serve both.
CREATE INDEX IF NOT EXISTS entity_geom ON entity USING GIST (geom);
CREATE INDEX IF NOT EXISTS entity_centroid ON entity USING GIST (centroid);
CREATE INDEX IF NOT EXISTS entity_published ON entity (programme_id, entity_type_code) WHERE status = 'published';
-- Free-text search over the name, without a separate search cluster.
CREATE INDEX IF NOT EXISTS entity_name_trgm ON entity USING GIN (name gin_trgm_ops);

CREATE TABLE IF NOT EXISTS entity_category (
    entity_id     uuid        NOT NULL REFERENCES entity (id) ON DELETE CASCADE,
    category_code text        NOT NULL,
    -- Opaque text validated against `core.programme_entity_type` through the API, not by
    -- a foreign key: the categories are core's, and duplicating them here is the drift
    -- this design removed.
    is_primary    boolean     NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now(),
    -- Mutable: which category is primary changes when a reviewer reclassifies.
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (entity_id, category_code)
);

-- At most one primary category per entity.
CREATE UNIQUE INDEX IF NOT EXISTS entity_category_one_primary
    ON entity_category (entity_id) WHERE is_primary;

CREATE TABLE IF NOT EXISTS entity_attachment (
    id         uuid        PRIMARY KEY,
    entity_id  uuid        NOT NULL REFERENCES entity (id) ON DELETE CASCADE,
    kind       text        NOT NULL DEFAULT 'photo',
    -- The object key in the content bucket. An id-derived key, never the user's filename.
    object_key text        NOT NULL,
    media_type text        NOT NULL,
    size_bytes bigint      NOT NULL,
    checksum   text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    confirmed_at timestamptz,
    CONSTRAINT entity_attachment_size_positive CHECK (size_bytes > 0)
);

CREATE INDEX IF NOT EXISTS entity_attachment_entity ON entity_attachment (entity_id);

-- append-only: a decision, with the time it was made. A revised decision is a new row.
CREATE TABLE IF NOT EXISTS entity_review (
    id          uuid        PRIMARY KEY,
    entity_id   uuid        NOT NULL REFERENCES entity (id) ON DELETE CASCADE,
    -- → ext:core
    reviewer_id uuid        NOT NULL,
    decision    text        NOT NULL,
    notes       text,
    decided_at  timestamptz NOT NULL DEFAULT now(),
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT entity_review_decision_known CHECK (decision IN ('approve', 'reject', 'needs_info'))
);

CREATE INDEX IF NOT EXISTS entity_review_entity ON entity_review (entity_id, decided_at DESC);

-- ───────────────────────────────────────────────────────────────────────────────
-- Provenance — where the data came from
-- ───────────────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS source (
    id             uuid        PRIMARY KEY,
    name           text        NOT NULL,
    adapter        text        NOT NULL,
    default_format text        NOT NULL,
    -- Licence and attribution are required: an entity derived from this source inherits
    -- them, and a catalogue that cannot state where it came from cannot be published.
    licence        text        NOT NULL,
    attribution    text        NOT NULL,
    homepage       text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT source_adapter_known CHECK (adapter IN ('PARKSERVE_US', 'OSM', 'GOVERNMENT_GIS', 'MANUAL')),
    CONSTRAINT source_format_known CHECK (default_format IN
        ('geojson', 'wfs_geojson', 'arcgis', 'kml', 'gpx', 'shapefile_zip', 'osm_pbf', 'csv_wkt'))
);

CREATE UNIQUE INDEX IF NOT EXISTS source_name ON source (name);

-- The exact bytes an import read. One row per fetch, with a feature count — the two
-- legacy tables for this (manifest and record) collapsed into one.
-- append-only: the bytes an import read are history. `retrieved_at` is when.
CREATE TABLE IF NOT EXISTS source_snapshot (
    id            uuid        PRIMARY KEY,
    source_id     uuid        NOT NULL REFERENCES source (id) ON DELETE CASCADE,
    checksum      text        NOT NULL,
    object_key    text        NOT NULL,
    feature_count integer     NOT NULL DEFAULT 0,
    retrieved_at  timestamptz NOT NULL DEFAULT now(),
    created_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT source_snapshot_feature_count_non_negative CHECK (feature_count >= 0)
);

CREATE INDEX IF NOT EXISTS source_snapshot_source ON source_snapshot (source_id, retrieved_at DESC);

-- append-only: provenance is a record of what was read. Re-running an import adds rows;
-- it never rewrites what an earlier run saw.
CREATE TABLE IF NOT EXISTS entity_provenance (
    entity_id         uuid        NOT NULL REFERENCES entity (id) ON DELETE CASCADE,
    snapshot_id       uuid        NOT NULL REFERENCES source_snapshot (id) ON DELETE CASCADE,
    -- The source's own identifier for the feature, which is what makes a merge auditable.
    source_feature_id text        NOT NULL,
    confidence        numeric(4, 3) NOT NULL DEFAULT 1.0,
    created_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (entity_id, snapshot_id, source_feature_id),
    CONSTRAINT entity_provenance_confidence_range CHECK (confidence BETWEEN 0 AND 1)
);

CREATE INDEX IF NOT EXISTS entity_provenance_snapshot ON entity_provenance (snapshot_id);

-- ───────────────────────────────────────────────────────────────────────────────
-- Imports
-- ───────────────────────────────────────────────────────────────────────────────

-- `review` is a status, not a stage: "awaiting an administrator" is not something the
-- pipeline does, and modelling it as a stage made the legacy state machine 11 states wide.
CREATE TABLE IF NOT EXISTS import_run (
    id           uuid        PRIMARY KEY,
    source_id    uuid        NOT NULL REFERENCES source (id) ON DELETE RESTRICT,
    programme_id uuid        NOT NULL,
    snapshot_id  uuid        REFERENCES source_snapshot (id) ON DELETE SET NULL,
    stage        text        NOT NULL DEFAULT 'upload',
    status       text        NOT NULL DEFAULT 'uploaded',
    counts       jsonb       NOT NULL DEFAULT '{}'::jsonb,
    started_at   timestamptz,
    finished_at  timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT import_run_stage_known CHECK (stage IN ('upload', 'normalise', 'validate', 'conflate', 'publish')),
    CONSTRAINT import_run_status_known CHECK (status IN ('uploaded', 'running', 'review', 'published', 'failed', 'cancelled'))
);

CREATE INDEX IF NOT EXISTS import_run_open
    ON import_run (created_at DESC) WHERE status IN ('uploaded', 'running', 'review');

CREATE TABLE IF NOT EXISTS import_candidate (
    id                uuid        PRIMARY KEY,
    run_id            uuid        NOT NULL REFERENCES import_run (id) ON DELETE CASCADE,
    source_feature_id text        NOT NULL,
    payload           jsonb       NOT NULL,
    geom              geometry(Geometry, 4326),
    state             text        NOT NULL DEFAULT 'pending',
    reject_reason     text,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT import_candidate_state_known
        CHECK (state IN ('pending', 'valid', 'invalid', 'duplicate', 'promoted', 'rejected'))
);

CREATE UNIQUE INDEX IF NOT EXISTS import_candidate_source_feature
    ON import_candidate (run_id, source_feature_id);
-- The review screen filters by state, and a 4 000-feature run is reviewed that way.
CREATE INDEX IF NOT EXISTS import_candidate_review
    ON import_candidate (run_id, state);

-- append-only: an error from a run. Fixing the data creates a new run, not an edit here.
CREATE TABLE IF NOT EXISTS import_error (
    id           uuid        PRIMARY KEY,
    run_id       uuid        NOT NULL REFERENCES import_run (id) ON DELETE CASCADE,
    candidate_id uuid        REFERENCES import_candidate (id) ON DELETE CASCADE,
    severity     text        NOT NULL DEFAULT 'error',
    code         text        NOT NULL,
    message      text        NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT import_error_severity_known CHECK (severity IN ('error', 'warning'))
);

CREATE INDEX IF NOT EXISTS import_error_run ON import_error (run_id, severity);

CREATE TABLE IF NOT EXISTS conflation_candidate (
    id           uuid        PRIMARY KEY,
    run_id       uuid        NOT NULL REFERENCES import_run (id) ON DELETE CASCADE,
    candidate_id uuid        NOT NULL REFERENCES import_candidate (id) ON DELETE CASCADE,
    entity_id    uuid        REFERENCES entity (id) ON DELETE SET NULL,
    score        numeric(4, 3) NOT NULL,
    reason       jsonb       NOT NULL DEFAULT '{}'::jsonb,
    resolution   text        NOT NULL DEFAULT 'unresolved',
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT conflation_candidate_score_range CHECK (score BETWEEN 0 AND 1),
    CONSTRAINT conflation_candidate_resolution_known
        CHECK (resolution IN ('new', 'merge', 'skip', 'unresolved'))
);

-- One decision per candidate, which is what makes "publish refuses while anything is
-- unresolved" a query rather than a scan.
CREATE UNIQUE INDEX IF NOT EXISTS conflation_candidate_one_per_candidate
    ON conflation_candidate (candidate_id);

CREATE TABLE IF NOT EXISTS upload_session (
    id         uuid        PRIMARY KEY,
    source_id  uuid        NOT NULL REFERENCES source (id) ON DELETE RESTRICT,
    object_key text        NOT NULL,
    size_bytes bigint      NOT NULL,
    checksum   text        NOT NULL,
    state      text        NOT NULL DEFAULT 'open',
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT upload_session_state_known CHECK (state IN ('open', 'completed', 'aborted', 'expired')),
    CONSTRAINT upload_session_size_positive CHECK (size_bytes > 0)
);

-- The 1 GiB ceiling from libs/limits, enforced here as a second line of defence: a bug
-- in session creation must not let an unbounded object into the bucket.
ALTER TABLE upload_session DROP CONSTRAINT IF EXISTS upload_session_size_ceiling;
ALTER TABLE upload_session ADD CONSTRAINT upload_session_size_ceiling
    CHECK (size_bytes <= 1073741824);

CREATE INDEX IF NOT EXISTS upload_session_expiry
    ON upload_session (expires_at) WHERE state = 'open';

-- append-only: a part that arrived, with its ETag. Re-uploading a part replaces the row
-- only by aborting the session, which deletes it.
CREATE TABLE IF NOT EXISTS upload_part (
    session_id  uuid        NOT NULL REFERENCES upload_session (id) ON DELETE CASCADE,
    part_number integer     NOT NULL,
    etag        text        NOT NULL,
    size_bytes  bigint      NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (session_id, part_number),
    CONSTRAINT upload_part_number_range CHECK (part_number BETWEEN 1 AND 10000),
    CONSTRAINT upload_part_size_positive CHECK (size_bytes > 0)
);

-- A schedule is a row, not a cron loop in a container. The unique slot index in
-- `ops.job` is what makes it fire once per period across replicas; this table holds the
-- definition.
CREATE TABLE IF NOT EXISTS import_schedule (
    id          uuid        PRIMARY KEY,
    source_id   uuid        NOT NULL REFERENCES source (id) ON DELETE CASCADE,
    programme_id uuid,
    cron        text        NOT NULL,
    enabled     boolean     NOT NULL DEFAULT true,
    last_run_at timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS import_schedule_source
    ON import_schedule (source_id, cron);

-- ───────────────────────────────────────────────────────────────────────────────
-- Audit
-- ───────────────────────────────────────────────────────────────────────────────

-- append-only: the audit trail. `occurred_at` is when, and nothing edits it.
CREATE TABLE IF NOT EXISTS geodata_audit_event (
    id          uuid        PRIMARY KEY,
    entity_id   uuid        REFERENCES entity (id) ON DELETE SET NULL,
    -- → ext:core
    actor_id    uuid,
    action      text        NOT NULL,
    before      jsonb,
    after       jsonb,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS geodata_audit_event_entity ON geodata_audit_event (entity_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS geodata_audit_event_actor ON geodata_audit_event (actor_id, occurred_at DESC);
