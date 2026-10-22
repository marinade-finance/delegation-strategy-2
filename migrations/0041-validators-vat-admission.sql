-- NULL on rows stored before this migration.
ALTER TABLE validators ADD COLUMN bls_pubkey TEXT DEFAULT NULL;
ALTER TABLE validators ADD COLUMN vat_admitted BOOL DEFAULT NULL;
-- no_bls_key, no_stake, low_balance or below_cutoff. NULL when admitted.
ALTER TABLE validators ADD COLUMN vat_rejection_reason TEXT DEFAULT NULL;
ALTER TABLE validators ADD COLUMN vat_runway_epochs NUMERIC DEFAULT NULL;
