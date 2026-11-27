-- The `ops` schema: the operational pattern every service shares.
--
-- Written once and used by both implementations. `tools/gen_ops_sql.py` copies this
-- file into libs/ops/go/sql/ and libs/ops/rust/sql/ so each module can embed it; SQL
-- is not language-specific, so the DDL is genuinely shared while the code is not.
--
-- Applied by each service to its own database, in its own migrations. There is no
-- shared database: the outbox row must be written in the same transaction as the
-- domain change, which means it has to live in the database that owns that change.
--
-- Every statement is idempotent (`IF NOT EXISTS`), so re-running is a no-op. That is
-- an acceptance criterion, and it is why the migration runner can be simple: it
-- records what it applied, and the statements tolerate being applied twice anyway.

CREATE SCHEMA IF NOT EXISTS ops;

-- The event log. One row per fact, written inside the domain transaction so an event
-- can never exist without its change, or a change without its event.
CREATE TABLE IF NOT EXISTS ops.outbox_event (
    -- Monotonic per database and NOT contiguous: sequences are consumed by aborted
    -- transactions too. Consumers treat it as a cursor, never as a count.
    seq          bigserial    PRIMARY KEY,
    id           uuid         NOT NULL UNIQUE,
    type         text         NOT NULL,
    subject      text         NOT NULL,
    partition    integer      NOT NULL,
    payload      jsonb        NOT NULL,
    created_at   timestamptz  NOT NULL DEFAULT now(),
    -- Null until the relay has published this event. The relay publishes in seq
    -- order under a partition lease, which is what keeps per-aggregate ordering
    -- intact when several relay replicas run.
    published_at timestamptz
);

-- The relay's hot path: unpublished rows for one partition, in seq order.
CREATE INDEX IF NOT EXISTS outbox_event_unpublished
    ON ops.outbox_event (partition, seq)
    WHERE published_at IS NULL;

-- Retention: trims by age, and the ack floor trims by seq.
CREATE INDEX IF NOT EXISTS outbox_event_created_at
    ON ops.outbox_event (created_at);

-- Consumers report how far they have applied, so the producer can trim without
-- trimming past a live consumer.
CREATE TABLE IF NOT EXISTS ops.event_ack (
    consumer       text        NOT NULL,
    source         text        NOT NULL,
    last_acked_seq bigint      NOT NULL DEFAULT 0,
    reported_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (consumer, source)
);

-- The relay's lease and high-water mark, one row per partition. Up to PARTITIONS
-- relay replicas publish concurrently, each owning different partitions, which is
-- what keeps per-aggregate ordering without making the relay a single leader.
CREATE TABLE IF NOT EXISTS ops.outbox_partition (
    partition   integer     PRIMARY KEY,
    -- Null when no relay holds the partition. The lease expires rather than being
    -- released on a crash, so a killed replica's partitions are reclaimed by the
    -- next one to ask.
    lease_owner text,
    lease_until timestamptz,
    -- The highest seq published for this partition. The relay publishes in seq order
    -- and never moves this backwards, so a consumer reading the feed cannot see a
    -- later event for an aggregate before an earlier one.
    last_seq    bigint      NOT NULL DEFAULT 0,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT outbox_partition_range CHECK (partition >= 0)
);

-- This service's read position in a *producer's* log, one row per partition so that
-- up to PARTITIONS pullers can work concurrently and ownership is dynamic.
CREATE TABLE IF NOT EXISTS ops.consumer_checkpoint (
    consumer     text        NOT NULL,
    source       text        NOT NULL,
    partition    integer     NOT NULL,
    last_seq     bigint      NOT NULL DEFAULT 0,
    lease_owner  text,
    lease_until  timestamptz,
    PRIMARY KEY (consumer, source, partition)
);

-- Work with competing consumers. The queue is a table, not a broker: N replicas claim
-- disjoint rows with FOR UPDATE SKIP LOCKED, which is exactly what a durable pull
-- consumer did, minus the server (ADR-0004).
CREATE TABLE IF NOT EXISTS ops.job (
    id            uuid        PRIMARY KEY,
    kind          text        NOT NULL,
    payload       jsonb       NOT NULL,
    -- queued | leased | done | dead
    state         text        NOT NULL DEFAULT 'queued',
    attempt       integer     NOT NULL DEFAULT 0,
    max_attempts  integer     NOT NULL DEFAULT 8,
    available_at  timestamptz NOT NULL DEFAULT now(),
    lease_until   timestamptz,
    lease_owner   text,
    -- Set for work that must happen once per period (a nightly sweep). The partial
    -- unique index below is what makes a schedule fire once per slot across replicas
    -- rather than once per replica.
    scheduled_for timestamptz,
    last_error    text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT job_state_known CHECK (state IN ('queued', 'leased', 'done', 'dead')),
    CONSTRAINT job_attempts_sane CHECK (attempt >= 0 AND max_attempts > 0)
);

-- The claim query's index. Partial, because a claim never looks at settled rows.
CREATE INDEX IF NOT EXISTS job_claimable
    ON ops.job (state, available_at)
    WHERE state IN ('queued', 'leased');

-- One row per (kind, slot). A second insert for the same slot is a no-op rather than
-- a second job, which is how a cron in a container becomes a schedule in a table.
CREATE UNIQUE INDEX IF NOT EXISTS job_scheduled_slot
    ON ops.job (kind, scheduled_for)
    WHERE scheduled_for IS NOT NULL;

-- Request-level idempotency for mutating API calls.
CREATE TABLE IF NOT EXISTS ops.idempotency_record (
    scope        text        NOT NULL,
    key          text        NOT NULL,
    -- SHA-256 of the request body. A replay with a different body is a different
    -- operation reusing a key, which is a caller bug the API reports rather than
    -- silently executing twice.
    request_hash text        NOT NULL,
    response     jsonb       NOT NULL,
    status       integer     NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    PRIMARY KEY (scope, key)
);

CREATE INDEX IF NOT EXISTS idempotency_record_expiry ON ops.idempotency_record (expires_at);

-- Terminal failures, for both jobs and events. One table, so the redrive tool has one
-- shape to read rather than one per mechanism.
CREATE TABLE IF NOT EXISTS ops.dead_letter (
    id           uuid        PRIMARY KEY,
    source_table text        NOT NULL,
    ref_id       uuid        NOT NULL,
    reason       text        NOT NULL,
    payload      jsonb       NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS dead_letter_created_at ON ops.dead_letter (created_at);
