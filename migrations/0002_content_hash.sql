-- SHA-256 of the uploaded bytes, kept by Postgres so it can never drift from `data`.
ALTER TABLE documents ADD COLUMN sha256 BYTEA GENERATED ALWAYS AS (sha256(data)) STORED;

-- Earlier duplicate uploads: keep the newest, whose output comes from the latest converter.
DELETE FROM documents d USING documents newer
WHERE d.sha256 = newer.sha256 AND (d.created_at, d.id) < (newer.created_at, newer.id);

CREATE UNIQUE INDEX documents_sha256_key ON documents (sha256);
