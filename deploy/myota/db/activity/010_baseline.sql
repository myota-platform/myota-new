-- Baseline schema for `myota_activity`. Owned by `activity`.
--
-- Derived from docs/architecture/data-model.md, section "`myota_activity` — owned by
-- `activity`". `tools/check_db_schema.py` asserts every table the document names here
-- appears in this file.
--
-- No spatial columns. An activation stores the entity id and the name and elevation it
-- captured at start, and asks `geodata` for geometry through its API when a map needs it.
-- That capture is deliberate: a later rename in `geodata` must not rewrite a log.

-- ───────────────────────────────────────────────────────────────────────────────
-- Activations and QSOs
-- ───────────────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS activation (
    id                    uuid        PRIMARY KEY,
    -- → ext:core
    account_id            uuid        NOT NULL,
    callsign              text        NOT NULL,
    -- → ext:geodata
    entity_id             uuid        NOT NULL,
    -- Captured at start. A rename in `geodata` does not rewrite this: a log records what
    -- was true when it was written.
    entity_name           text        NOT NULL,
    entity_elevation_m    numeric(7, 2),
    -- → ext:core
    programme_id          uuid        NOT NULL,
    started_at            timestamptz NOT NULL,
    ended_at              timestamptz,
    status                text        NOT NULL DEFAULT 'active',
    qso_count             integer     NOT NULL DEFAULT 0,
    points                integer     NOT NULL DEFAULT 0,
    -- Set when the referenced aggregate is closed in its owning service. The row is kept
    -- rather than deleted: history must survive the other service's lifecycle.
    detached_at           timestamptz,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT activation_status_known CHECK (status IN ('active', 'closed', 'cancelled')),
    CONSTRAINT activation_times_ordered CHECK (ended_at IS NULL OR ended_at >= started_at),
    CONSTRAINT activation_counts_non_negative CHECK (qso_count >= 0 AND points >= 0),
    CONSTRAINT activation_closed_has_end
        CHECK (status <> 'closed' OR ended_at IS NOT NULL)
);

-- One open activation per callsign. Two open activations from one operator is a
-- double-counting bug, not a feature — a partial unique index makes it impossible
-- rather than merely discouraged.
CREATE UNIQUE INDEX IF NOT EXISTS activation_one_open_per_callsign
    ON activation (callsign) WHERE status = 'active';
CREATE INDEX IF NOT EXISTS activation_account
    ON activation (account_id, started_at DESC);
CREATE INDEX IF NOT EXISTS activation_entity
    ON activation (entity_id, started_at DESC);

CREATE TABLE IF NOT EXISTS qso (
    id                uuid        PRIMARY KEY,
    activation_id     uuid        NOT NULL REFERENCES activation (id) ON DELETE CASCADE,
    worked_callsign   text        NOT NULL,
    band              text        NOT NULL,
    mode              text        NOT NULL,
    rst_sent          text,
    rst_received      text,
    worked_at         timestamptz NOT NULL,
    points            integer     NOT NULL DEFAULT 0,
    source            text        NOT NULL DEFAULT 'live',
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT qso_source_known CHECK (source IN ('live', 'adif', 'csv')),
    CONSTRAINT qso_points_non_negative CHECK (points >= 0)
);

-- The activation's log, in the order it was logged.
CREATE INDEX IF NOT EXISTS qso_activation ON qso (activation_id, worked_at);
CREATE INDEX IF NOT EXISTS qso_worked_callsign ON qso (worked_callsign);

CREATE TABLE IF NOT EXISTS qso_correction (
    id           uuid        PRIMARY KEY,
    qso_id       uuid        NOT NULL REFERENCES qso (id) ON DELETE CASCADE,
    -- → ext:core
    requested_by uuid        NOT NULL,
    changes      jsonb       NOT NULL,
    reason       text,
    status       text        NOT NULL DEFAULT 'requested',
    reviewed_by  uuid,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    decided_at   timestamptz,
    decision_note text,
    CONSTRAINT qso_correction_status_known CHECK (status IN ('requested', 'approved', 'rejected'))
);

-- The reviewer's queue: open corrections only, oldest first.
CREATE INDEX IF NOT EXISTS qso_correction_open
    ON qso_correction (created_at) WHERE status = 'requested';

CREATE TABLE IF NOT EXISTS log_import (
    id            uuid        PRIMARY KEY,
    activation_id uuid        NOT NULL REFERENCES activation (id) ON DELETE CASCADE,
    format        text        NOT NULL,
    object_key    text        NOT NULL,
    size_bytes    bigint      NOT NULL,
    checksum      text,
    accepted      integer     NOT NULL DEFAULT 0,
    rejected      integer     NOT NULL DEFAULT 0,
    status        text        NOT NULL DEFAULT 'queued',
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT log_import_format_known CHECK (format IN ('adif', 'csv')),
    CONSTRAINT log_import_status_known CHECK (status IN ('queued', 'running', 'completed', 'failed')),
    CONSTRAINT log_import_counts_non_negative CHECK (accepted >= 0 AND rejected >= 0)
);

-- The 32 MiB ceiling from libs/limits. A log is text; anything larger is a dataset, and
-- those go through the import pipeline instead.
ALTER TABLE log_import DROP CONSTRAINT IF EXISTS log_import_size_ceiling;
ALTER TABLE log_import ADD CONSTRAINT log_import_size_ceiling CHECK (size_bytes <= 33554432);

CREATE INDEX IF NOT EXISTS log_import_activation ON log_import (activation_id, created_at DESC);

-- append-only: the outcome of one line in a file. A re-import is a new import, so rows
-- are never revised.
CREATE TABLE IF NOT EXISTS log_import_row (
    id          uuid        PRIMARY KEY,
    import_id   uuid        NOT NULL REFERENCES log_import (id) ON DELETE CASCADE,
    line_number integer     NOT NULL,
    outcome     text        NOT NULL,
    message     text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT log_import_row_outcome_known CHECK (outcome IN ('accepted', 'rejected')),
    CONSTRAINT log_import_row_line_positive CHECK (line_number > 0)
);

-- One outcome per line, so re-running a parser cannot double the row set.
CREATE UNIQUE INDEX IF NOT EXISTS log_import_row_unique
    ON log_import_row (import_id, line_number);
-- The rejected rows are what the participant fixes, so they get their own index.
CREATE INDEX IF NOT EXISTS log_import_row_rejected
    ON log_import_row (import_id) WHERE outcome = 'rejected';

-- ───────────────────────────────────────────────────────────────────────────────
-- Awards
-- ───────────────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS award_definition (
    id           uuid        PRIMARY KEY,
    -- → ext:core
    programme_id uuid        NOT NULL,
    code         text        NOT NULL,
    name         text        NOT NULL,
    -- A condition tree, evaluated by the award.evaluate job. Not a foreign key to a rule
    -- table: a rule engine's conditions are a document, and normalising them into rows
    -- is how the legacy system ended up with four tables describing one rule.
    rules        jsonb       NOT NULL,
    status       text        NOT NULL DEFAULT 'draft',
    version      integer     NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT award_definition_status_known CHECK (status IN ('draft', 'active', 'archived')),
    CONSTRAINT award_definition_version_positive CHECK (version > 0)
);

-- A published definition is versioned: editing the rules of an award already issued
-- would reinterpret a certificate somebody holds.
CREATE UNIQUE INDEX IF NOT EXISTS award_definition_code_version
    ON award_definition (programme_id, code, version);

CREATE TABLE IF NOT EXISTS award_request (
    id           uuid        PRIMARY KEY,
    award_id     uuid        NOT NULL REFERENCES award_definition (id) ON DELETE RESTRICT,
    -- → ext:core
    account_id   uuid        NOT NULL,
    status       text        NOT NULL DEFAULT 'requested',
    note         text,
    submitted_at timestamptz NOT NULL DEFAULT now(),
    decided_at   timestamptz,
    decision_note text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT award_request_status_known CHECK (status IN ('requested', 'approved', 'rejected', 'issued'))
);

-- One open request per account per award: a participant clicking twice is not two attempts.
CREATE UNIQUE INDEX IF NOT EXISTS award_request_one_open
    ON award_request (award_id, account_id) WHERE status IN ('requested', 'approved');
CREATE INDEX IF NOT EXISTS award_request_queue
    ON award_request (submitted_at) WHERE status = 'requested';

-- A materialised evaluation, not a per-request calculation: a rule engine running inside
-- a GET is how a page load becomes a transaction.
CREATE TABLE IF NOT EXISTS award_progress (
    award_id     uuid        NOT NULL REFERENCES award_definition (id) ON DELETE CASCADE,
    -- → ext:core
    account_id   uuid        NOT NULL,
    satisfied    boolean     NOT NULL DEFAULT false,
    progress     jsonb       NOT NULL DEFAULT '{}'::jsonb,
    evaluated_at timestamptz NOT NULL DEFAULT now(),
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (award_id, account_id)
);

CREATE INDEX IF NOT EXISTS award_progress_satisfied
    ON award_progress (award_id) WHERE satisfied;

CREATE TABLE IF NOT EXISTS award_issuance (
    id              uuid        PRIMARY KEY,
    award_id        uuid        NOT NULL REFERENCES award_definition (id) ON DELETE RESTRICT,
    -- → ext:core
    account_id      uuid        NOT NULL,
    request_id      uuid        REFERENCES award_request (id) ON DELETE SET NULL,
    issued_at       timestamptz NOT NULL DEFAULT now(),
    -- The object key in the records bucket. The certificate itself is in object storage,
    -- never in the database: a PDF in a row is a row every query pays for.
    certificate_key text        NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS award_issuance_account ON award_issuance (account_id, issued_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS award_issuance_certificate ON award_issuance (certificate_key);

-- ───────────────────────────────────────────────────────────────────────────────
-- Statistics — materialised, with the time they were built
-- ───────────────────────────────────────────────────────────────────────────────

-- append-only: a snapshot is an immutable measurement with the time it was built.
-- Rebuilding writes a new row; the old one is what makes a historical number defensible.
CREATE TABLE IF NOT EXISTS statistic_snapshot (
    id       uuid        PRIMARY KEY,
    scope    text        NOT NULL,
    scope_id uuid        NOT NULL,
    period   text        NOT NULL DEFAULT 'all_time',
    payload  jsonb       NOT NULL,
    built_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT statistic_snapshot_scope_known CHECK (scope IN ('account', 'programme', 'entity')),
    CONSTRAINT statistic_snapshot_period_known CHECK (period IN ('all_time', 'year', 'month'))
);

-- The newest snapshot for a scope is the one a read returns, so the index is descending
-- and scoped rather than global.
CREATE INDEX IF NOT EXISTS statistic_snapshot_latest
    ON statistic_snapshot (scope, scope_id, built_at DESC);

-- append-only: a ranking inside an immutable snapshot. The snapshot is the unit of
-- change; an entry is never edited.
CREATE TABLE IF NOT EXISTS leaderboard_entry (
    snapshot_id uuid        NOT NULL REFERENCES statistic_snapshot (id) ON DELETE CASCADE,
    rank        integer     NOT NULL,
    -- → ext:core
    account_id  uuid        NOT NULL,
    -- Captured in the snapshot, so a rename does not reshuffle a historical leaderboard.
    display_name text,
    value       numeric     NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (snapshot_id, rank),
    CONSTRAINT leaderboard_entry_rank_positive CHECK (rank > 0)
);

CREATE INDEX IF NOT EXISTS leaderboard_entry_account ON leaderboard_entry (snapshot_id, account_id);
