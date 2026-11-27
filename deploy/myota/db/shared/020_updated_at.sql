-- `updated_at` is maintained by trigger, not by application code.
--
-- From docs/architecture/data-model.md: "Every table carries `created_at timestamptz
-- not null default now()` and `updated_at timestamptz not null default now()`; `updated_at`
-- is maintained by trigger, not by application code."
--
-- Not by application code, because "every INSERT and UPDATE remembers it" is a rule
-- fifty-one call sites have to keep and one will forget. A trigger cannot be forgotten.
--
-- Written as one loop rather than fifty-one CREATE TRIGGER statements: a hand-written
-- list goes stale the moment a table is added, and the failure is silent — a new table
-- whose updated_at never moves looks correct until somebody sorts by it. The loop reads
-- the catalogue, so it covers tables added later without an edit here.

CREATE OR REPLACE FUNCTION ops_set_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    -- now() is the transaction start time, so two rows changed by one statement get the
    -- same value. That is what makes `WHERE updated_at > :since` a usable cursor: it
    -- cannot miss a row that committed in the same instant as one it returned.
    NEW.updated_at := now();
    RETURN NEW;
END;
$$;

DO $$
DECLARE
    target record;
BEGIN
    -- Every base table in a non-system schema that has an `updated_at` column and no
    -- trigger yet. `pg_trigger` check first so this is idempotent: re-running on a
    -- database where the triggers exist must change nothing, which is an acceptance
    -- criterion for the whole migration set.
    FOR target IN
        SELECT n.nspname AS schema_name, c.relname AS table_name
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        JOIN pg_attribute a ON a.attrelid = c.oid
        WHERE c.relkind = 'r'
          AND a.attname = 'updated_at'
          AND NOT a.attisdropped
          AND n.nspname NOT IN ('pg_catalog', 'information_schema')
          AND n.nspname NOT LIKE 'pg_toast%'
          -- Matched on the trigger name alone. The obvious refinement — also checking
          -- `tgattr` names the column — needs a cast from `int2vector` that does not
          -- exist, and the first version of this file failed on exactly that. Every
          -- trigger this function creates is named `set_updated_at` and fires on
          -- `updated_at`, so the name is the whole check.
          AND NOT EXISTS (
              SELECT 1 FROM pg_trigger t
              WHERE t.tgrelid = c.oid AND t.tgname = 'set_updated_at')
        ORDER BY n.nspname, c.relname
    LOOP
        EXECUTE format(
            'CREATE TRIGGER set_updated_at BEFORE UPDATE ON %I.%I
             FOR EACH ROW EXECUTE FUNCTION ops_set_updated_at()',
            target.schema_name, target.table_name);
    END LOOP;
END;
$$;
