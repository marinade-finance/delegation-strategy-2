ALTER TABLE validators RENAME COLUMN client_id TO client_id_raw;
ALTER TABLE validators
  ADD COLUMN client_id   INTEGER NULL,
  ADD COLUMN client_name TEXT    NULL;

ALTER TABLE versions RENAME COLUMN client_id TO client_id_raw;
ALTER TABLE versions
  ADD COLUMN client_id   INTEGER NULL,
  ADD COLUMN client_name TEXT    NULL;

-- Discarded rather than backfilled: mixing pre-rename data in would leave rows only partly populated.
UPDATE validators SET client_id_raw = NULL, client_vendor = NULL, client_lineage = NULL
WHERE client_id_raw IS NOT NULL OR client_vendor IS NOT NULL OR client_lineage IS NOT NULL;

UPDATE versions SET client_id_raw = NULL, client_vendor = NULL, client_lineage = NULL
WHERE client_id_raw IS NOT NULL OR client_vendor IS NOT NULL OR client_lineage IS NOT NULL;
