ALTER TABLE validators
  DROP COLUMN client_name,
  DROP COLUMN client_vendor,
  DROP COLUMN client_lineage;

ALTER TABLE versions
  DROP COLUMN client_name,
  DROP COLUMN client_vendor,
  DROP COLUMN client_lineage;
