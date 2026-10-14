-- Chunk budget the stored chunks were cut with; NULL for rows chunked before it was recorded.
ALTER TABLE documents ADD COLUMN chunk_tokens INT;
