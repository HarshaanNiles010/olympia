-- IF NOT EXISTS: databases created before migrations already have this table.
CREATE TABLE IF NOT EXISTS documents (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    filename     TEXT NOT NULL,
    content_type TEXT,
    data         BYTEA NOT NULL,
    markdown     TEXT NOT NULL,
    chunks       JSONB NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER TABLE documents ADD COLUMN IF NOT EXISTS metadata JSONB;

-- Tables from before UUID ids: convert in place. Existing rows get new random ids.
DO $$ BEGIN
    IF (SELECT data_type FROM information_schema.columns
        WHERE table_schema = current_schema() AND table_name = 'documents' AND column_name = 'id') = 'bigint' THEN
        ALTER TABLE documents ALTER COLUMN id DROP DEFAULT;
        ALTER TABLE documents ALTER COLUMN id TYPE UUID USING gen_random_uuid();
        ALTER TABLE documents ALTER COLUMN id SET DEFAULT gen_random_uuid();
        DROP SEQUENCE IF EXISTS documents_id_seq;
    END IF;
END $$;
