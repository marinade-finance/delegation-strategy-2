-- NULL on rows stored before this migration.
ALTER TABLE validators ADD COLUMN bls_pubkey TEXT DEFAULT NULL;
ALTER TABLE cluster_info ADD COLUMN vat_lamports_per_epoch NUMERIC DEFAULT NULL;
