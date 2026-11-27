-- Baseline schema for `myota_core`. Owned by `core`; no other service reads it.
--
-- Derived from docs/architecture/data-model.md, section "`myota_core` — owned by
-- `core`". `tools/check_db_schema.py` asserts that every table the document names for
-- this database appears here, so the two cannot drift silently.
--
-- Applied by the migration Job, not by a replica at startup (AGENTS.md rule 12). The
-- runner applies each migration once and records it, and every statement here is
-- `IF NOT EXISTS`, so re-applying changes nothing even if the ledger were lost.
--
-- Identity, authorisation, programme configuration, and the notification ledger.

-- citext, because an email address is one address regardless of case and a unique
-- index over `text` would let `Ana@example.org` and `ana@example.org` both register.
CREATE EXTENSION IF NOT EXISTS citext;

-- ───────────────────────────────────────────────────────────────────────────────
-- Accounts and credentials
-- ───────────────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS account (
    -- Supplied by the application, not by a default. A default would make it easy to
    -- insert a row whose id the caller never learned, which for an account means a row
    -- nobody can log in to. Seed and test data may use gen_random_uuid().
    id                uuid        PRIMARY KEY,
    display_name      text        NOT NULL,
    email             citext      NOT NULL,
    participation_type text       NOT NULL,
    status            text        NOT NULL DEFAULT 'pending',
    email_verified    boolean     NOT NULL DEFAULT false,
    -- Monotonic registration order, for a stable "newest first" listing that does not
    -- depend on a timestamp two requests could share.
    insertion_order   bigint      GENERATED ALWAYS AS IDENTITY,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    -- A closed account is kept: its activations are somebody's history.
    closed_at         timestamptz,
    CONSTRAINT account_participation_type_known CHECK (participation_type IN ('OPERATOR', 'SWL')),
    CONSTRAINT account_status_known CHECK (status IN ('pending', 'active', 'closed')),
    CONSTRAINT account_email_is_lowercase CHECK (email = lower(email::text)::citext)
);

-- One account per address. `citext` makes the comparison case-insensitive, which is
-- what a person expects when they type their address with a different capitalisation.
CREATE UNIQUE INDEX IF NOT EXISTS account_email_key ON account (email);
CREATE INDEX IF NOT EXISTS account_insertion_order ON account (insertion_order DESC);
-- The sweeper looks for pending accounts older than seven days.
CREATE INDEX IF NOT EXISTS account_pending ON account (created_at) WHERE status = 'pending';

CREATE TABLE IF NOT EXISTS account_credential (
    account_id         uuid        PRIMARY KEY REFERENCES account (id) ON DELETE CASCADE,
    password_hash      text,
    password_changed_at timestamptz,
    failed_attempts    integer     NOT NULL DEFAULT 0,
    locked_until       timestamptz,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    -- Credential-less accounts exist (an admin provisioned by another admin, an account
    -- that only signs in with a passkey). A hash without a timestamp or vice versa is a
    -- bug: it means one half of the change was lost.
    CONSTRAINT account_credential_pair CHECK (
        (password_hash IS NULL) = (password_changed_at IS NULL))
);

CREATE TABLE IF NOT EXISTS account_mfa (
    account_id   uuid        NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    method       text        NOT NULL,
    -- A reference into the secret store, never the secret itself: this table is read by
    -- an admin console and backed up, and a TOTP seed in either is a second factor that
    -- is no longer a factor.
    secret_ref   text,
    confirmed_at timestamptz,
    last_used_at timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, method),
    CONSTRAINT account_mfa_method_known CHECK (method IN ('totp', 'webauthn')),
    CONSTRAINT account_mfa_confirmed_has_secret CHECK (confirmed_at IS NULL OR secret_ref IS NOT NULL)
);

-- append-only: a recovery code is consumed, never edited. `used_at` records the
-- consumption; the row itself does not change.
CREATE TABLE IF NOT EXISTS account_recovery_code (
    id         uuid        PRIMARY KEY,
    account_id uuid        NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    code_hash  text        NOT NULL,
    used_at    timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS account_recovery_code_account ON account_recovery_code (account_id) WHERE used_at IS NULL;

-- One table for all four email flows: verification, password reset, address change.
-- Four tables would be four copies of the same token logic.
CREATE TABLE IF NOT EXISTS verification_token (
    -- The hash is the key: the raw token is never stored, so a database read cannot be
    -- turned into an account takeover.
    token_hash text        PRIMARY KEY,
    account_id uuid        NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    purpose    text        NOT NULL,
    -- The address the token was issued for. Re-checked on use, so a token cannot be
    -- applied to an address that changed after it was sent.
    email      citext      NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at    timestamptz,
    issued_ip  inet,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT verification_token_purpose_known CHECK (purpose IN ('verify_email', 'reset_password', 'change_email'))
);

CREATE INDEX IF NOT EXISTS verification_token_account_purpose ON verification_token (account_id, purpose);

CREATE TABLE IF NOT EXISTS auth_session (
    id                 uuid        PRIMARY KEY,
    account_id         uuid        NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    -- The column that keeps the two logins separate: a participant session is not an
    -- administrator session, and the gateway refuses it on /v1/admin/**.
    audience           text        NOT NULL,
    refresh_token_hash text        NOT NULL,
    access_jti         uuid        NOT NULL,
    expires_at         timestamptz NOT NULL,
    -- Idle and absolute expiry are different limits; the absolute one bounds a leaked
    -- refresh token even if it is used regularly.
    absolute_expires_at timestamptz NOT NULL,
    revoked_at         timestamptz,
    user_agent         text,
    remote_addr        inet,
    last_seen_at       timestamptz NOT NULL DEFAULT now(),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT auth_session_audience_known CHECK (audience IN ('participant', 'admin'))
);

CREATE UNIQUE INDEX IF NOT EXISTS auth_session_refresh_hash ON auth_session (refresh_token_hash);
CREATE INDEX IF NOT EXISTS auth_session_account_open
    ON auth_session (account_id) WHERE revoked_at IS NULL;

-- append-only: a revocation is a fact with a time. There is nothing to update.
CREATE TABLE IF NOT EXISTS auth_revocation (
    jti        uuid        PRIMARY KEY,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Expiry drives the cleanup; a denylist that grows forever is a leak.
CREATE INDEX IF NOT EXISTS auth_revocation_expires ON auth_revocation (expires_at);

-- Per-account lockout. Not the same thing as edge rate limiting: this counter must be
-- global and authoritative, so it lives in the database where every replica sees it.
CREATE TABLE IF NOT EXISTS login_rate_limit (
    key_hash          text        PRIMARY KEY,
    window_started_at timestamptz NOT NULL DEFAULT now(),
    request_count     integer     NOT NULL DEFAULT 0,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);

-- append-only: an audit row. `occurred_at` says when it happened, which is the field a
-- reader asks for; nothing rewrites it.
CREATE TABLE IF NOT EXISTS security_event (
    id         uuid        PRIMARY KEY,
    account_id uuid        REFERENCES account (id) ON DELETE SET NULL,
    event_type text        NOT NULL,
    payload    jsonb       NOT NULL DEFAULT '{}'::jsonb,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS security_event_account ON security_event (account_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS security_event_type ON security_event (event_type, occurred_at DESC);

-- ───────────────────────────────────────────────────────────────────────────────
-- Callsigns and roles
-- ───────────────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS callsign (
    id               uuid        PRIMARY KEY,
    account_id       uuid        NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    value            text        NOT NULL,
    lifecycle_status text        NOT NULL DEFAULT 'UNVERIFIED',
    verified_at      timestamptz,
    valid_from       date,
    valid_to         date,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT callsign_status_known CHECK (lifecycle_status IN ('UNVERIFIED', 'VERIFIED', 'RETIRED'))
);

-- The same callsign twice on one account is a duplicate claim; the same callsign on two
-- accounts is contested and handled by the verification workflow, not by the schema.
CREATE UNIQUE INDEX IF NOT EXISTS callsign_account_value ON callsign (account_id, value);
CREATE INDEX IF NOT EXISTS callsign_value ON callsign (value);
-- The reviewer's queue.
CREATE INDEX IF NOT EXISTS callsign_awaiting_verification
    ON callsign (created_at) WHERE lifecycle_status = 'UNVERIFIED';

CREATE TABLE IF NOT EXISTS callsign_evidence (
    id            uuid        PRIMARY KEY,
    callsign_id   uuid        NOT NULL REFERENCES callsign (id) ON DELETE CASCADE,
    evidence_type text        NOT NULL,
    source        text,
    checksum      text,
    metadata      jsonb       NOT NULL DEFAULT '{}'::jsonb,
    status        text        NOT NULL DEFAULT 'pending',
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT callsign_evidence_status_known CHECK (status IN ('pending', 'accepted', 'rejected'))
);

CREATE INDEX IF NOT EXISTS callsign_evidence_callsign ON callsign_evidence (callsign_id);

CREATE TABLE IF NOT EXISTS account_primary_callsign (
    account_id  uuid        PRIMARY KEY REFERENCES account (id) ON DELETE CASCADE,
    -- UNIQUE: a callsign can be primary for at most one account, which is what makes
    -- "the primary callsign" a well-defined label rather than a claim two rows share.
    callsign_id uuid        NOT NULL UNIQUE REFERENCES callsign (id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS role_definition (
    role_code    text        PRIMARY KEY,
    display_name text        NOT NULL,
    scopes       text[]      NOT NULL DEFAULT '{}',
    is_system    boolean     NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS account_role (
    id                uuid        PRIMARY KEY,
    account_id        uuid        NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    role_code         text        NOT NULL REFERENCES role_definition (role_code),
    -- Null means the grant is not scoped to a programme.
    programme_id      uuid,
    jurisdiction_code text,
    entity_type_code  text,
    scopes            jsonb       NOT NULL DEFAULT '{}'::jsonb,
    valid_to          timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);

-- Open grants only: the authorisation check never looks at expired rows, and a partial
-- index keeps the hot path small as history accumulates.
CREATE INDEX IF NOT EXISTS account_role_open
    ON account_role (account_id) WHERE valid_to IS NULL;
CREATE INDEX IF NOT EXISTS account_role_programme
    ON account_role (programme_id) WHERE valid_to IS NULL;

CREATE TABLE IF NOT EXISTS approver_scope (
    id               uuid        PRIMARY KEY,
    programme_id     uuid        NOT NULL,
    entity_type_code text        NOT NULL,
    account_id       uuid        NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS approver_scope_unique
    ON approver_scope (programme_id, entity_type_code, account_id);

-- ───────────────────────────────────────────────────────────────────────────────
-- Programmes
-- ───────────────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS programme (
    id                  uuid        PRIMARY KEY,
    slug                text        NOT NULL UNIQUE,
    name                text        NOT NULL,
    status              text        NOT NULL DEFAULT 'draft',
    default_entity_type text,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    published_at        timestamptz,
    archived_at         timestamptz,
    CONSTRAINT programme_status_known CHECK (status IN ('draft', 'active', 'archived'))
);

CREATE TABLE IF NOT EXISTS programme_entity_type (
    programme_id    uuid        NOT NULL REFERENCES programme (id) ON DELETE CASCADE,
    category_code   text        NOT NULL,
    label           text        NOT NULL,
    -- Which geometry types this category accepts, so an import cannot put a polygon
    -- where a summit point belongs.
    geometry_policy jsonb       NOT NULL DEFAULT '{}'::jsonb,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (programme_id, category_code)
);

CREATE TABLE IF NOT EXISTS programme_content (
    id            uuid        PRIMARY KEY,
    programme_id  uuid        NOT NULL REFERENCES programme (id) ON DELETE CASCADE,
    kind          text        NOT NULL,
    locale        text        NOT NULL DEFAULT 'en',
    body_markdown text        NOT NULL,
    status        text        NOT NULL DEFAULT 'draft',
    published_at  timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT programme_content_kind_known CHECK (kind IN ('rules', 'about', 'faq')),
    CONSTRAINT programme_content_status_known CHECK (status IN ('draft', 'published'))
);

CREATE UNIQUE INDEX IF NOT EXISTS programme_content_live
    ON programme_content (programme_id, kind, locale) WHERE status = 'published';

-- Immutable once published: an award already evaluated used these rules, and editing
-- them in place would change the basis of a certificate somebody holds.
CREATE TABLE IF NOT EXISTS programme_policy_version (
    id           uuid        PRIMARY KEY,
    programme_id uuid        NOT NULL REFERENCES programme (id) ON DELETE CASCADE,
    version      integer     NOT NULL,
    rules        jsonb       NOT NULL,
    published_at timestamptz NOT NULL DEFAULT now(),
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT programme_policy_version_positive CHECK (version > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS programme_policy_version_unique
    ON programme_policy_version (programme_id, version);

CREATE TABLE IF NOT EXISTS programme_policy_draft (
    id           uuid        PRIMARY KEY,
    programme_id uuid        NOT NULL REFERENCES programme (id) ON DELETE CASCADE,
    version      integer     NOT NULL,
    rules        jsonb       NOT NULL,
    status       text        NOT NULL DEFAULT 'draft',
    reviewer_id  uuid        REFERENCES account (id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    decided_at   timestamptz,
    CONSTRAINT programme_policy_draft_status_known
        CHECK (status IN ('draft', 'in_review', 'approved', 'rejected')),
    CONSTRAINT programme_policy_draft_version_positive CHECK (version > 0)
);

CREATE INDEX IF NOT EXISTS programme_policy_draft_review
    ON programme_policy_draft (programme_id) WHERE status = 'in_review';

-- Administrative geography for role scoping. NOT the spatial catalogue: geometry lives
-- in `myota_geo`, and a second spatial table here would be the duplication this design
-- removes.
CREATE TABLE IF NOT EXISTS jurisdiction (
    id        uuid        PRIMARY KEY,
    code      text        NOT NULL UNIQUE,
    name      text        NOT NULL,
    parent_id uuid        REFERENCES jurisdiction (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- ───────────────────────────────────────────────────────────────────────────────
-- Notifications — one ledger for every channel
-- ───────────────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS notification (
    id             uuid        PRIMARY KEY,
    account_id     uuid        REFERENCES account (id) ON DELETE SET NULL,
    -- An allowlisted template id, never caller-supplied body text: otherwise the
    -- notification system is an open relay for whoever can reach the API.
    template       text        NOT NULL,
    channel        text        NOT NULL,
    payload        jsonb       NOT NULL DEFAULT '{}'::jsonb,
    status         text        NOT NULL DEFAULT 'queued',
    attempts       integer     NOT NULL DEFAULT 0,
    -- The SMTP message id, used to correlate a bounce back to this row.
    provider_id    text,
    failure_reason text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    sent_at        timestamptz,
    read_at        timestamptz,
    CONSTRAINT notification_channel_known CHECK (channel IN ('email', 'in_app')),
    CONSTRAINT notification_status_known
        CHECK (status IN ('queued', 'sending', 'delivered', 'failed', 'dead', 'suppressed')),
    CONSTRAINT notification_attempts_bounded CHECK (attempts BETWEEN 0 AND 8)
);

CREATE INDEX IF NOT EXISTS notification_pending
    ON notification (created_at) WHERE status IN ('queued', 'sending');
CREATE INDEX IF NOT EXISTS notification_account_inbox
    ON notification (account_id, created_at DESC) WHERE channel = 'in_app';
CREATE INDEX IF NOT EXISTS notification_provider ON notification (provider_id) WHERE provider_id IS NOT NULL;

-- append-only: one row per delivery attempt. An attempt that happened is history.
CREATE TABLE IF NOT EXISTS notification_delivery_attempt (
    id                uuid        PRIMARY KEY,
    notification_id   uuid        NOT NULL REFERENCES notification (id) ON DELETE CASCADE,
    attempt           integer     NOT NULL,
    outcome           text        NOT NULL,
    provider_response text,
    attempted_at      timestamptz NOT NULL DEFAULT now(),
    created_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT notification_delivery_attempt_outcome_known
        CHECK (outcome IN ('accepted', 'transient_failure', 'permanent_failure')),
    CONSTRAINT notification_delivery_attempt_positive CHECK (attempt > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS notification_delivery_attempt_unique
    ON notification_delivery_attempt (notification_id, attempt);

-- append-only: received from the provider, never edited.
CREATE TABLE IF NOT EXISTS notification_bounce (
    id              uuid        PRIMARY KEY,
    notification_id uuid        REFERENCES notification (id) ON DELETE SET NULL,
    type            text        NOT NULL,
    code            text,
    diagnostic      text,
    received_at     timestamptz NOT NULL DEFAULT now(),
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT notification_bounce_type_known CHECK (type IN ('hard', 'soft', 'complaint'))
);

CREATE INDEX IF NOT EXISTS notification_bounce_notification ON notification_bounce (notification_id);

-- Keyed by address, not by notification: the worker checks it before sending, so there
-- is no foreign key and the row survives the account being deleted. A suppressed
-- address that comes back when the account is removed is a suppression that never
-- worked.
CREATE TABLE IF NOT EXISTS email_suppression (
    email       citext      PRIMARY KEY,
    reason      text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    -- Mutable: a suppression is released, not deleted, so the history survives.
    updated_at  timestamptz NOT NULL DEFAULT now(),
    released_at timestamptz,
    CONSTRAINT email_suppression_reason_known CHECK (reason IN ('hard_bounce', 'complaint', 'manual'))
);

CREATE INDEX IF NOT EXISTS email_suppression_active ON email_suppression (email) WHERE released_at IS NULL;
